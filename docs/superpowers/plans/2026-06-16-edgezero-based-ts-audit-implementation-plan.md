# EdgeZero-Based Trusted Server Audit CLI Implementation Plan

**Date:** 2026-06-16
**Status:** Approved implementation plan
**Spec:** `docs/superpowers/specs/2026-06-16-edgezero-based-ts-audit-design.md`
**Depends on:** base CLI pass from
`docs/superpowers/specs/2026-06-16-edgezero-based-ts-cli-design.md`

## Current baseline

The base CLI pass has added the host-target `trusted-server-cli` crate with:

```text
crates/trusted-server-cli/
  Cargo.toml
  src/args.rs
  src/config_command.rs
  src/edgezero_delegate.rs
  src/error.rs
  src/lib.rs
  src/main.rs
  src/run.rs
```

Important existing shapes to preserve:

- The binary is `ts`.
- The implementation is gated to non-wasm targets in `lib.rs` and `main.rs`.
- `run_from_env()` parses process args and wires production services.
- `run_with_io()` supports testable invocation with injected writers.
- `run::dispatch()` currently injects an `EdgeZeroDelegate` for lifecycle/config
  push tests.
- `config_command.rs` already embeds `trusted-server.example.toml` for
  `config init`.
- `trusted-server.example.toml` now uses `example.com` sentinel values rather
  than the old `test-publisher.com` values.
- `.gitignore` already ignores `trusted-server.toml`, but does not yet ignore
  `js-assets.toml`.

The old implementation to port from is on `feature/ts-cli`:

```text
crates/trusted-server-cli/src/audit.rs
crates/trusted-server-cli/src/audit/analyzer.rs
crates/trusted-server-cli/src/audit/browser_collector.rs
crates/trusted-server-cli/src/audit/collector.rs
```

This plan recreates that behavior on top of the new base CLI structure, while
applying the spec's tightening around output preflight, deterministic merge
behavior, and EdgeZero separation.

## Decisions locked for this plan

- `ts audit` is Trusted Server-owned, not an EdgeZero delegate.
- No `--adapter`, `--manifest`, `--store`, `--local`, `--dry-run`, or `--json`
  options are added to audit v1.
- The command writes local draft artifacts only; it never provisions, pushes,
  deploys, or contacts platform APIs.
- Preserve the old command surface:
  - `ts audit <url>`;
  - `--js-assets <path>`;
  - `--config <path>`;
  - `--no-js-assets`;
  - `--no-config`;
  - `--force`.
- Preserve the old artifact schema exactly enough that existing
  `js-assets.toml` readers do not need a migration.
- Improve over the old implementation by preflighting selected output paths
  before launching the browser and before writing any file.
- Use a fake collector in tests; unit tests must not require Chrome/Chromium.
- Browser smoke tests, if added, must be ignored by default or feature-gated.
- Generated `trusted-server.toml` is a draft. It may still fail production
  validation until the operator replaces placeholders and reviews settings.
- Do not write rendered HTML, inline script bodies, cookies, storage, request
  bodies, or response bodies to artifacts.
- Keep all browser automation dependencies host-only under
  `trusted-server-cli`.
- Follow repository error/logging style: `error-stack::Report`, no `println!`,
  output through injected `Write` handles in testable code.

## Definition of done

- `ts audit [options] <url>` appears in clap help and dispatches correctly.
- URL validation accepts only `http` and `https` URLs.
- Default outputs are `js-assets.toml` and `trusted-server.toml`.
- `--no-js-assets` and `--no-config` work individually.
- Passing both no-output flags fails before browser collection.
- Existing outputs are rejected without `--force` before browser collection.
- If any selected output path conflicts, no selected file is written.
- Browser collector launches an isolated headless Chrome/Chromium session.
- Browser collector captures final URL, title, rendered HTML, DOM scripts, and
  script resource timing entries.
- Navigation failures and non-`200..399` main-document statuses fail clearly.
- Page settle timeout continues with a warning.
- Analyzer merges HTML, DOM, and resource-timing script evidence.
- Assets and detected integrations are deduplicated and sorted deterministically.
- First-party/third-party classification matches the spec's host relationship
  heuristic.
- Integration detectors match the old v1 detector set.
- `js-assets.toml` serializes the specified schema.
- Draft config generation patches current `trusted-server.example.toml`
  sentinels, uses the final redirected URL, and appends manual-review comments.
- `ts audit` does not invoke any `EdgeZeroDelegate` or platform API.
- `.gitignore` ignores the default `js-assets.toml` artifact.
- CLI guide / getting-started docs mention the audit command and Chrome
  requirement.
- Focused unit tests pass.
- Host-target CLI tests pass.
- Formatting passes.

## Proposed module layout

Add audit as an internal host-only module under the existing CLI crate:

```text
crates/trusted-server-cli/src/
  audit.rs
  audit/
    analyzer.rs
    browser_collector.rs
    collector.rs
```

Responsibilities:

| File                         | Responsibility                                                                |
| ---------------------------- | ----------------------------------------------------------------------------- |
| `args.rs`                    | Add `Command::Audit(AuditArgs)` and parse audit flags.                        |
| `run.rs`                     | Dispatch audit via an injectable collector and stdout writer.                 |
| `audit.rs`                   | Command orchestration, output planning, file writes, draft config generation. |
| `audit/collector.rs`         | `CollectedPage` data structs and `AuditCollector` trait.                      |
| `audit/analyzer.rs`          | Convert `CollectedPage` to `AuditArtifact`; detection/classification.         |
| `audit/browser_collector.rs` | Production Chrome/Chromium collector.                                         |
| `Cargo.toml`                 | Add host-only audit dependencies.                                             |
| `.gitignore`                 | Ignore default `js-assets.toml`.                                              |
| docs                         | Document command usage and draft status.                                      |

## Data model sketch

Port these old public/internal shapes with doc comments as needed for clippy:

```rust
pub struct AuditArgs {
    pub url: String,
    pub js_assets: Option<PathBuf>,
    pub config: Option<PathBuf>,
    pub no_js_assets: bool,
    pub no_config: bool,
    pub force: bool,
}

pub trait AuditCollector {
    fn collect_page(&self, target_url: &Url) -> CliResult<CollectedPage>;
}

pub struct CollectedPage {
    pub requested_url: String,
    pub final_url: String,
    pub page_title: Option<String>,
    pub html: String,
    pub script_tags: Vec<CollectedScriptTag>,
    pub network_requests: Vec<CollectedRequest>,
    pub warnings: Vec<String>,
}

pub struct AuditArtifact {
    pub audited_url: String,
    pub page_title: Option<String>,
    pub js_asset_count: usize,
    pub third_party_asset_count: usize,
    pub detected_integrations: Vec<DetectedIntegration>,
    pub assets: Vec<AuditedAsset>,
    pub warnings: Vec<String>,
}
```

Keep serialization compatible with the old artifact:

- `AssetParty` serializes with `#[serde(rename_all = "kebab-case")]`.
- `AuditedAsset.integration` remains `Option<String>`.
- `page_title` remains `Option<String>`.
- No `schema_version` in v1.

## Service injection shape

The current `dispatch()` injects only an `EdgeZeroDelegate`. To test audit
without launching Chrome, extend the dispatcher to inject both platform and audit
services.

Preferred shape:

```rust
struct CliServices<'a> {
    edgezero: &'a mut dyn EdgeZeroDelegate,
    audit: &'a dyn AuditCollector,
}

fn dispatch(
    args: Args,
    services: &mut CliServices<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> CliResult<()>;
```

Production setup in `run_from_env()` and `run_with_io()`:

```rust
let mut edgezero = ProductionEdgeZeroDelegate;
let audit = BrowserAuditCollector;
let mut services = CliServices {
    edgezero: &mut edgezero,
    audit: &audit,
};
```

Tests can use:

```rust
let mut edgezero = FakeEdgeZeroDelegate::default();
let audit = FakeAuditCollector::new(collected_page);
let mut services = CliServices {
    edgezero: &mut edgezero,
    audit: &audit,
};
```

This keeps the no-EdgeZero requirement testable: after dispatching `Command::Audit`,
assert fake EdgeZero lifecycle/push calls are empty.

If introducing `CliServices` feels too large, an acceptable smaller alternative
is `dispatch_with_audit_collector(args, delegate, collector, out, err)` used by
production and tests. Avoid global mutable test hooks.

## Dependencies

Add only host-target CLI dependencies.

Likely additions to root `[workspace.dependencies]`:

```toml
chromiumoxide = "<chosen-compatible-version>"
scraper = "0.21" # or current compatible version
```

Likely additions to `crates/trusted-server-cli/Cargo.toml` under
`target.'cfg(not(target_arch = "wasm32"))'.dependencies`:

```toml
chromiumoxide = { workspace = true }
futures = { workspace = true }
regex = { workspace = true }
scraper = { workspace = true }
tokio = { workspace = true }
url = { workspace = true }
which = { workspace = true }
```

Existing workspace dependencies already include `futures`, `regex`, `tokio`,
`url`, and `which`. Confirm `tokio` features are sufficient for the browser
collector:

- current workspace features include `rt`, `time`, `macros`, `io-util`, and
  `sync`;
- browser collector needs current-thread runtime and timers;
- if `chromiumoxide` requires extra Tokio features, add only the minimum
  host-safe features needed.

Dependency constraints:

- Do not add these dependencies to runtime crates.
- Do not make `trusted-server-core` depend on browser automation or HTML
  scraping crates.
- Keep the CLI crate wasm stub compiling by leaving all real audit modules under
  `#[cfg(not(target_arch = "wasm32"))]` via `lib.rs` module gating.

## Stage 1 — Add CLI argument surface

Files:

- `crates/trusted-server-cli/src/args.rs`
- `crates/trusted-server-cli/src/run.rs`

Steps:

1. Add `Command::Audit(AuditArgs)` to `args.rs`.
2. Add `AuditArgs` with:
   - positional `url: String`;
   - `#[arg(long)] js_assets: Option<PathBuf>`;
   - `#[arg(long)] config: Option<PathBuf>`;
   - `#[arg(long)] no_js_assets: bool`;
   - `#[arg(long)] no_config: bool`;
   - `#[arg(long)] force: bool`.
3. Use clap's default kebab-case flag names, so the struct field `js_assets`
   maps to `--js-assets`.
4. Add parser tests:
   - parses default audit URL;
   - parses all custom options;
   - `--no-js-assets` and `--no-config` can each parse;
   - audit does not accept `--adapter`.
5. Add a dispatch match arm that calls `audit::run_audit()` with the injected
   collector.
6. Ensure existing delegate command parser tests remain unchanged.

Do not implement browser collection in this stage.

## Stage 2 — Add audit module scaffold and output planning

Files:

- `crates/trusted-server-cli/src/lib.rs`
- `crates/trusted-server-cli/src/audit.rs`
- `crates/trusted-server-cli/src/audit/collector.rs`

Steps:

1. Register `mod audit;` in `lib.rs` under the existing non-wasm module gate.
2. Add collector data structs and `AuditCollector` trait.
3. Add `AuditOutputPlan` in `audit.rs`:

   ```rust
   struct AuditOutputPlan {
       js_assets_path: Option<PathBuf>,
       config_path: Option<PathBuf>,
   }
   ```

4. Add `parse_audit_url(value: &str) -> CliResult<Url>`.
5. Add `resolve_output_plan(args: &AuditArgs) -> CliResult<AuditOutputPlan>`.
6. Rules for `resolve_output_plan()`:
   - reject both `no_js_assets` and `no_config`;
   - default JS asset path to `js-assets.toml` unless disabled;
   - default config path to `trusted-server.toml` unless disabled;
   - resolve relative paths against `std::env::current_dir()`;
   - preserve absolute paths;
   - reject existing selected paths unless `force`;
   - create no directories yet, or create only after all selected paths pass the
     conflict check.
7. Add `prepare_output_paths(plan)` or integrate directory creation after
   successful preflight.
8. Tests:
   - URL parsing accepts HTTP/HTTPS;
   - URL parsing rejects `file:`, `data:`, `chrome:`;
   - both no-output flags reject with a clear message;
   - default and custom paths resolve as expected;
   - existing file fails without `--force`;
   - existing file passes with `--force`;
   - one conflicting path prevents all writes.

Implementation note: keep path planning separate from browser collection so a
fake collector can record whether it was called. Use that to prove conflicts
short-circuit before collection.

## Stage 3 — Port analyzer and artifact schema

Files:

- `crates/trusted-server-cli/src/audit.rs`
- `crates/trusted-server-cli/src/audit/analyzer.rs`

Steps:

1. Add serializable artifact structs in `audit.rs`:
   - `AssetParty`;
   - `AuditedAsset`;
   - `DetectedIntegration`;
   - `AuditArtifact`;
   - `AuditOutputs`.
2. Port `analyze_collected_page()` from the old branch.
3. Preserve these analysis inputs:
   - rendered HTML `<script>` tags;
   - browser-collected `document.scripts` entries;
   - browser resource timing entries with resource type `script`,
     case-insensitive.
4. Preserve these analysis outputs:
   - final audited URL;
   - title from browser title or rendered `<title>` fallback;
   - sorted/deduplicated assets;
   - sorted detected integrations;
   - warnings.
5. Implement deterministic merge behavior:
   - key assets by absolute URL string in `BTreeMap`;
   - if an existing asset has `integration = None` and a later source detects an
     integration, update the existing row;
   - if an existing asset already has an integration, keep the first detected
     integration.
6. Preserve host-party classification:
   - exact host match is first-party;
   - dot-boundary subdomain relationship in either direction is first-party;
   - otherwise third-party.
7. Preserve warning behavior:
   - carry collector warnings through;
   - add redirect warning when requested and final URL differ;
   - add malformed HTML script URL warnings;
   - ignore malformed browser/resource URLs.
8. Serialize artifact with `toml::to_string_pretty()`.
9. Tests:
   - title fallback/preference;
   - redirect warning;
   - HTML + DOM + resource timing merge;
   - dedupe;
   - dedupe updates integration from later evidence;
   - relative script resolution against final URL;
   - malformed HTML script URL warning;
   - non-script resource timing ignored;
   - party classification cases;
   - deterministic order;
   - TOML serialization has expected fields and kebab-case parties.

## Stage 4 — Port integration detection

Files:

- `crates/trusted-server-cli/src/audit/analyzer.rs`

Steps:

1. Port URL detector behavior for:
   - `google_tag_manager`;
   - `gpt`;
   - `didomi`;
   - `datadome`;
   - `permutive`;
   - `lockr`;
   - `prebid`.
2. Port inline detector behavior:
   - GTM regex: `\bGTM-[A-Z0-9]+\b`;
   - case-insensitive markers for the other v1 integration IDs.
3. Keep the detector implementation small and easy to extend. Prefer constants
   or focused helper functions over spreading string literals through the
   analyzer loop.
4. Add `extract_gtm_container_id(artifact: &AuditArtifact) -> Option<String>`.
5. Evidence precedence:
   - first evidence per integration ID wins;
   - GTM container ID evidence is preferred naturally when encountered first;
   - if only URL evidence exists, extract GTM ID from the asset URL when
     possible.
6. Tests:
   - GTM inline snippet detection;
   - GTM URL ID extraction;
   - GPT URL detection;
   - Didomi URL detection;
   - DataDome URL detection;
   - Permutive URL detection;
   - Lockr URL detection;
   - Prebid URL detection;
   - inline marker detection is case-insensitive;
   - unrelated URLs/scripts do not detect integrations.

Testing note: detector tests may use public vendor host/path patterns only where
needed to prove the detector constants. Do not use real publisher/customer
domains.

## Stage 5 — Implement draft config generation against current template

Files:

- `crates/trusted-server-cli/src/audit.rs`
- maybe `crates/trusted-server-cli/src/config_command.rs`

Steps:

1. Reuse the same embedded `trusted-server.example.toml` content as
   `config init`.
   - Current constant is private in `config_command.rs`.
   - Either make a small `pub(crate) const EXAMPLE_CONFIG` available, or create a
     tiny shared `template` helper to avoid duplicate `include_str!` paths.
2. Generate draft config from the final audited URL, not the requested URL.
3. Patch current template sentinels:

   ```toml
   domain = "example.com"
   cookie_domain = ".example.com"
   origin_url = "https://origin.example.com"
   ```

4. Set:
   - `publisher.domain = <final host without port>`;
   - `publisher.cookie_domain = ".<final host>"`;
   - `publisher.origin_url = <final origin with non-default port preserved>`.
5. Auto-enable only these integrations:
   - GPT: set `[integrations.gpt].enabled = true`;
   - Didomi: set `[integrations.didomi].enabled = true`;
   - DataDome: set `[integrations.datadome].enabled = true`;
   - GTM: set `[integrations.google_tag_manager].enabled = true` and replace
     `container_id` only when a usable `GTM-...` ID is extracted.
6. For GTM without a container ID, do not enable. Add a manual-review comment.
7. For Permutive, Lockr, and Prebid, add manual-review comments.
8. Do not add platform/provider/EdgeZero sections.
9. Do not infer secrets, consent, auction bidders, request signing, stores, or
   EdgeZero environment overlays.
10. Keep the generated config readable and parseable TOML.

Implementation detail for robust replacements:

- Avoid global `enabled = false` replacement.
- Use section-aware replacement helpers, for example:
  - find `[integrations.gpt]` section and replace that section's first
    `enabled = false` line;
  - find `[integrations.google_tag_manager]` and replace both `enabled` and
    `container_id` lines while preserving `upstream_url`.
- If a required sentinel/section is missing, fail with an audit error explaining
  which template section could not be updated.

Tests:

- final redirected URL drives config fields;
- non-default port is preserved in `origin_url`;
- GPT/Didomi/DataDome auto-enable;
- GTM with ID auto-enables and sets container ID;
- GTM without ID adds manual-review comment and stays disabled;
- Prebid/Permutive/Lockr manual-review comments;
- generated config parses as TOML;
- no EdgeZero/provider sections are added.

## Stage 6 — Implement production browser collector

Files:

- `crates/trusted-server-cli/src/audit/browser_collector.rs`

Steps:

1. Add `BrowserAuditCollector` implementing `AuditCollector`.
2. Inside the sync trait method, create a current-thread Tokio runtime:

   ```rust
   tokio::runtime::Builder::new_current_thread()
       .enable_all()
       .build()
   ```

3. Find browser executable:
   - PATH candidates:
     - `google-chrome`;
     - `google-chrome-stable`;
     - `chromium`;
     - `chromium-browser`;
     - `chrome`;
     - `Google Chrome`;
     - `Google Chrome for Testing`.
   - macOS fallback paths:
     - `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`;
     - `/Applications/Chromium.app/Contents/MacOS/Chromium`;
     - `/Applications/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing`.
   - Linux fallback paths:
     - `/usr/bin/google-chrome`;
     - `/usr/bin/google-chrome-stable`;
     - `/usr/bin/chromium`;
     - `/usr/bin/chromium-browser`;
     - `/snap/bin/chromium`.
4. Launch headless Chrome/Chromium with a fresh profile.
5. Spawn the chromiumoxide handler task and drain events until completion/error.
6. Navigate to the target URL.
7. Wait for the main document response.
8. Validate navigation response:
   - missing request => error;
   - failure text => error;
   - missing response => error;
   - status in `200..400` => ok;
   - otherwise error with status, status text, and response URL.
9. Wait for settle:
   - poll interval `250ms`;
   - quiet period `750ms`;
   - max wait `6s`;
   - ready state must be `complete`;
   - resource count must be stable for the quiet period.
10. On settle timeout, push warning and continue.
11. Collect:
    - `page.url()`;
    - `page.get_title()`;
    - `page.content()`;
    - `document.scripts` with `src` and inline text;
    - `performance.getEntriesByType('resource')` with URL and initiator type.
12. Map DOM scripts into `CollectedScriptTag`, filtering empty inline text.
13. Map resource entries into `CollectedRequest` with `method = "GET"` and
    `status = None`.
14. Close the browser and surface close errors.
15. Abort/await the handler task as in the old implementation.

Tests:

- keep helper functions small enough to unit-test without launching a browser;
- test successful/failed navigation status helper;
- test browser fallback list helpers if practical;
- do not run real browser launch in default unit tests.

Watch point: the spec requires an isolated fresh session. Confirm the chosen
chromiumoxide config uses a temporary profile or does not reuse the user's
profile. If needed, explicitly create a `TempDir` user data directory that lives
for the browser session.

## Stage 7 — Command orchestration and writes

Files:

- `crates/trusted-server-cli/src/audit.rs`
- `crates/trusted-server-cli/src/run.rs`

Steps:

1. Implement:

   ```rust
   pub fn run_audit(
       args: &AuditArgs,
       collector: &dyn AuditCollector,
       out: &mut dyn Write,
   ) -> CliResult<()>;
   ```

2. Order of operations:
   - validate no-output flags;
   - parse URL;
   - resolve/preflight output plan;
   - collect page via injected collector;
   - analyze collected page;
   - serialize `js-assets.toml`;
   - build draft config;
   - create parent dirs for selected outputs;
   - write selected outputs;
   - print success summary.
3. Use a helper `write_audit_outputs()` that receives already-built content and
   selected paths.
4. If directory creation or file write fails for one output, return an IO-flavored
   CLI error with the path context.
5. Success summary must match the spec:

   ```text
   Audited <final-url>
   Title: <title-or-unknown>
   JS assets: <count>
   Third-party assets: <count>
   Detected integrations: <none-or-comma-list>
   Wrote: <paths>
   ```

6. Ensure integrations in summary are sorted by ID because artifact generation
   is sorted.
7. Keep warnings in `js-assets.toml`; no need to print warning list in v1.
8. Add orchestration tests with a fake collector:
   - writes both default outputs;
   - writes only config with `--no-js-assets`;
   - writes only assets with `--no-config`;
   - output conflict prevents collector call;
   - collector error writes no files;
   - summary includes expected values;
   - fake EdgeZero delegate remains unused.

## Stage 8 — Docs and gitignore

Files likely touched:

- `.gitignore`
- `README.md`
- `docs/guide/getting-started.md`
- possibly create or update `docs/guide/cli.md` if the base CLI pass added it
- `docs/superpowers/specs/2026-06-16-edgezero-based-ts-audit-design.md` only if
  implementation reveals a spec correction is needed

Steps:

1. Add `js-assets.toml` to `.gitignore` next to `trusted-server.toml`.
2. Update README quick start only if it currently documents the new CLI command
   set.
3. Add operator docs for:
   - Chrome/Chromium requirement;
   - default generated files;
   - `--no-config`, `--no-js-assets`, `--force`;
   - draft config status and need to run `ts config validate` after editing;
   - audit has no `--adapter` and does not push config.
4. Ensure docs use example domains only.
5. Run docs formatting for touched Markdown.

## Stage 9 — Verification plan

Run focused checks after implementing audit:

```bash
cargo fmt --all -- --check
HOST_TARGET="$(rustc -vV | sed -n 's/^host: //p')"
cargo test --package trusted-server-cli --target "$HOST_TARGET"
cargo check --package trusted-server-cli --target "$HOST_TARGET"
```

Then run broader checks required by repository policy as time permits / before
handoff:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

If docs are touched:

```bash
cd docs && npx prettier --check .
```

If adding `chromiumoxide` or changing workspace dependencies causes wasm-target
resolution issues, first verify the host CLI target, then verify the workspace
wasm/default target to make sure host-only dependencies do not leak into runtime
crates.

No default verification command should require Chrome/Chromium to be installed.

## Test matrix summary

| Area            | Tests                                                                                |
| --------------- | ------------------------------------------------------------------------------------ |
| Args            | parse audit URL/options; reject unsupported audit flags via clap.                    |
| URL validation  | accept HTTP/HTTPS; reject file/data/chrome/malformed.                                |
| Output planning | defaults, custom paths, absolute/relative, force, conflicts, no-output reject.       |
| Analyzer        | title, redirects, merge, dedupe, sorting, party classification, warnings.            |
| Detection       | GTM, GPT, Didomi, DataDome, Permutive, Lockr, Prebid URL/inline evidence.            |
| Artifact        | TOML shape, optional fields, kebab-case party, deterministic output.                 |
| Draft config    | final URL fields, integration edits, manual-review comments, parseable TOML.         |
| Browser helpers | browser discovery fallback helpers, status validation, settle helper where possible. |
| Orchestration   | fake collector writes selected outputs, summary, no EdgeZero calls.                  |

## File-by-file implementation checklist

### `crates/trusted-server-cli/Cargo.toml`

- [ ] Add host-only audit dependencies.
- [ ] Add dev dependencies only if tests need them beyond existing `tempfile`.

### Root `Cargo.toml`

- [ ] Add workspace dependencies for `chromiumoxide` and `scraper` if chosen.
- [ ] Confirm dependency features remain host-only through crate-level target
      dependency declarations.

### `crates/trusted-server-cli/src/lib.rs`

- [ ] Add `mod audit;` behind `#[cfg(not(target_arch = "wasm32"))]`.

### `crates/trusted-server-cli/src/args.rs`

- [ ] Add `AuditArgs`.
- [ ] Add `Command::Audit(AuditArgs)`.
- [ ] Add parser tests.

### `crates/trusted-server-cli/src/run.rs`

- [ ] Add `CliServices` or equivalent multi-service dispatch injection.
- [ ] Wire production `BrowserAuditCollector`.
- [ ] Dispatch `Command::Audit` to `audit::run_audit()`.
- [ ] Add fake-collector orchestration tests.

### `crates/trusted-server-cli/src/config_command.rs`

- [ ] Expose the example config template as `pub(crate)` or move to a shared
      helper so audit and config init use the same bytes.

### `crates/trusted-server-cli/src/audit.rs`

- [ ] Define artifact structs.
- [ ] Implement URL parsing.
- [ ] Implement output planning/preflight.
- [ ] Implement draft config generation.
- [ ] Implement output writes and success summary.
- [ ] Add unit tests for config generation and output planning.

### `crates/trusted-server-cli/src/audit/collector.rs`

- [ ] Define collected page structs.
- [ ] Define `AuditCollector` trait.
- [ ] Add URL parsing helpers on `CollectedPage` if useful.

### `crates/trusted-server-cli/src/audit/analyzer.rs`

- [ ] Port analysis from old implementation.
- [ ] Port/organize integration detectors.
- [ ] Add analyzer and detector tests.

### `crates/trusted-server-cli/src/audit/browser_collector.rs`

- [ ] Implement browser discovery.
- [ ] Implement headless browser collection.
- [ ] Implement settle wait.
- [ ] Implement navigation response validation.
- [ ] Add browser-independent tests.

### `.gitignore`

- [ ] Add `js-assets.toml`.

### Docs

- [ ] Add/update CLI usage docs.
- [ ] Add Chrome/Chromium requirement.
- [ ] Add generated-file warnings.

## Risk register

| Risk                                                              | Mitigation                                                                                               |
| ----------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| `chromiumoxide` API/version drift from old branch                 | Pin a compatible version, port in a small stage, and keep browser code isolated.                         |
| Browser deps leak into wasm/default builds                        | Keep deps under CLI host-target dependency table and modules under non-wasm gate.                        |
| Output preflight still allows partial writes on late IO failure   | Preflight known conflicts first; document v1 does not require atomic replacement.                        |
| Text replacement breaks when template changes                     | Use section-aware replacement helpers and fail with explicit missing-section errors.                     |
| GTM is enabled without usable container ID                        | Implement explicit GTM ID extraction gate before enabling.                                               |
| Detector string literals use real publisher/customer data         | Use only public vendor detector constants and example domains in tests/docs.                             |
| Tests accidentally require Chrome                                 | Use fake collector for orchestration and unit-test helpers only.                                         |
| Existing base CLI dispatch tests become awkward with two services | Introduce a small `CliServices` wrapper and adapt tests once rather than adding globals.                 |
| Clippy warns about public items missing docs                      | Keep audit modules private where possible; add doc comments for any public crate-visible APIs as needed. |

## Approved implementation choices

The following choices were confirmed before implementation:

1. Use `chromiumoxide` again for v1 browser collection.
2. Add `js-assets.toml` to `.gitignore`.
3. Keep no `--browser` override in v1.
4. Use section-aware text edits against `trusted-server.example.toml` rather than
   parsed-TOML rewriting.
5. Add `CliServices` or equivalent service injection in `run.rs` so audit command
   tests can use a fake collector.
