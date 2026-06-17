# Trusted Server CLI — Page Audit and Config Bootstrap

**Date:** 2026-06-16
**Status:** Draft design
**Scope:** `ts audit` in the EdgeZero-backed Trusted Server product CLI
**Related context:**

- `docs/superpowers/specs/2026-06-16-edgezero-based-ts-cli-design.md`
- `docs/superpowers/plans/2026-06-16-trusted-server-cli-respec-context.md`
- Prior implementation on `feature/ts-cli`:
  - `crates/trusted-server-cli/src/lib.rs`
  - `crates/trusted-server-cli/src/audit.rs`
  - `crates/trusted-server-cli/src/audit/analyzer.rs`
  - `crates/trusted-server-cli/src/audit/browser_collector.rs`
  - `crates/trusted-server-cli/src/audit/collector.rs`
  - `docs/guide/cli.md`

---

## 1. Goal

Add `ts audit` back to the new EdgeZero-backed Trusted Server CLI as a
Trusted Server-specific browser audit and config-bootstrap command.

`ts audit` loads a public publisher page in a real headless Chrome/Chromium
browser, collects rendered script evidence, classifies JavaScript assets,
detects known Trusted Server integrations, and writes local draft artifacts:

```text
js-assets.toml       # page/script audit artifact
trusted-server.toml  # draft Trusted Server app config
```

The command is intentionally **not** an EdgeZero lifecycle command. It does not
provision, push config, deploy, build, serve, authenticate, or resolve platform
adapters. It runs inside the `ts` product CLI because its behavior is specific
to Trusted Server onboarding and integration discovery.

The rebuilt command should preserve the old `feature/ts-cli` user-facing
behavior unless this spec explicitly tightens it:

```bash
ts audit https://publisher.example

ts audit https://publisher.example --no-config

ts audit https://publisher.example \
  --js-assets audit/js-assets.toml \
  --config audit/trusted-server.toml
```

The audit output is a **starter draft**, not a production-ready deployment. The
operator must review the generated config, replace placeholders/secrets, validate
it with `ts config validate`, then push it through the separate EdgeZero-backed
config workflow.

---

## 2. Non-goals

The initial `ts audit` does **not** do any of the following:

- delegate to EdgeZero adapters;
- require `--adapter`;
- read, validate, generate, or modify `edgezero.toml`;
- push config-store entries;
- write secret-store entries;
- provision platform resources;
- infer platform store names or runtime config-store settings;
- validate that the generated `trusted-server.toml` is production-ready;
- replace manual publisher configuration review;
- crawl more than the requested page;
- run Lighthouse or performance scoring;
- inspect non-script asset classes as first-class artifacts;
- capture request/response bodies, cookies, local storage, session storage, or
  arbitrary page data;
- support authenticated pages, user profiles, or inherited browser cookies;
- provide a browser UI/headful mode;
- support remote browser execution;
- add a plugin system for third-party audit detectors;
- create tickets, docs pages, or reports beyond the local TOML artifacts;
- support JSON command output in v1.

---

## 3. Relationship to the EdgeZero-backed CLI

`ts audit` is a product-level onboarding command that lives beside the
EdgeZero-backed command surface defined in the base CLI spec:

```text
ts config init
ts config validate
ts config push --adapter <adapter>
ts auth login --adapter <adapter>
ts provision --adapter <adapter>
ts serve --adapter <adapter>
ts build --adapter <adapter>
ts deploy --adapter <adapter>
ts audit <url>
```

The boundary is:

| Command family                         | Owner                                     | Platform behavior |
| -------------------------------------- | ----------------------------------------- | ----------------- |
| `ts auth/provision/serve/build/deploy` | EdgeZero delegates                        | Yes               |
| `ts config push`                       | Trusted Server transform + EdgeZero write | Yes               |
| `ts audit`                             | Trusted Server CLI                        | No                |

`ts audit` may share generic CLI infrastructure with the base CLI crate:
argument parsing, path resolution, output helpers, error formatting, and
`trusted-server.example.toml` access. It must not share or introduce platform
adapter logic.

Implementation rule:

> Adding or changing `ts audit` must not require changes to EdgeZero adapter
> traits, EdgeZero platform manifests, or runtime platform stores.

---

## 4. Command surface

```bash
ts audit [options] <url>
```

| Argument / option    | Default               | Description                                             |
| -------------------- | --------------------- | ------------------------------------------------------- |
| `<url>`              | required              | Public `http` or `https` page URL to audit.             |
| `--js-assets <path>` | `js-assets.toml`      | Write the JavaScript asset audit artifact to this path. |
| `--config <path>`    | `trusted-server.toml` | Write the draft Trusted Server config to this path.     |
| `--no-js-assets`     | `false`               | Do not write the JavaScript asset audit artifact.       |
| `--no-config`        | `false`               | Do not write the draft Trusted Server config.           |
| `--force`            | `false`               | Overwrite existing output files.                        |

Rules:

- `<url>` must parse as a URL and must use the `http` or `https` scheme.
- Relative output paths resolve from the current working directory.
- Absolute output paths are used as-is.
- Parent directories are created for selected outputs.
- Existing output files are not overwritten unless `--force` is passed.
- `--no-js-assets` and `--no-config` may each be used alone.
- Passing both `--no-js-assets` and `--no-config` is an argument error because
  the command would have no local output to write.
- `--force` applies to every selected output path.
- There is no `--adapter`, `--manifest`, `--store`, `--local`, `--dry-run`, or
  `--json` option for audit in v1.

Recommended fail-fast order:

1. Parse arguments.
2. Validate `<url>`.
3. Resolve the selected output paths.
4. Preflight all selected output paths for overwrite conflicts.
5. Run the browser audit.
6. Build all output content in memory.
7. Write selected output files.
8. Print the success summary.

This improves the old implementation by avoiding a long browser run when output
paths are already known to be unwritable, and by avoiding partial writes when one
selected path conflicts.

---

## 5. Output file model

### 5.1 Generated files

By default, `ts audit` writes:

```text
js-assets.toml
trusted-server.toml
```

`js-assets.toml` is a local audit artifact. It contains URL inventory and
integration evidence for the audited page.

`trusted-server.toml` is a draft app config generated from
`trusted-server.example.toml` and patched with values inferred from the final
audited page URL and detected integrations.

### 5.2 Source control expectations

The default output paths may contain publisher hostnames, vendor inventory, and
configuration placeholders. They are operator-owned artifacts, not generic sample
files.

The repository should ignore the default operator-owned outputs:

```text
trusted-server.toml
js-assets.toml
```

Custom paths under an `audit/` directory are allowed. Project documentation
should warn operators to review generated audit artifacts before committing them,
because they may describe a real publisher page.

### 5.3 Draft config status

The generated config is expected to be syntactically valid TOML, but it is not
required to pass production validation immediately.

Reasons it may still fail `ts config validate`:

- the starter template can include placeholder secrets;
- detected integrations may need publisher-specific IDs or endpoints;
- non-detected integrations may still need manual enablement or disablement;
- publisher-specific consent, auction, proxy, and request-signing fields cannot
  be inferred reliably from one page load.

The success summary and docs must call it a draft.

---

## 6. Browser collection pipeline

`ts audit` uses a real headless Chrome/Chromium browser because a static HTML
fetch misses scripts injected by tag managers, consent managers, ad stacks, and
other runtime code.

The v1 collector preserves the old `feature/ts-cli` behavior:

1. Locate a local Chrome/Chromium executable.
2. Launch a fresh headless browser session.
3. Open a new page at `about:blank`.
4. Navigate to the requested URL.
5. Wait for the main document navigation response.
6. Reject failed navigations and non-success HTTP statuses.
7. Wait for the page to settle.
8. Read the final page URL.
9. Read the page title.
10. Read the rendered HTML.
11. Read `document.scripts` from the rendered DOM.
12. Read browser resource timing entries.
13. Close the browser.
14. Analyze the collected page data.

### 6.1 Browser executable resolution

The collector checks common Chrome/Chromium executable names on `PATH`, then
standard local install locations for supported host operating systems.

The old implementation checked PATH names equivalent to:

```text
google-chrome
google-chrome-stable
chromium
chromium-browser
chrome
Google Chrome
Google Chrome for Testing
```

It also checked common macOS and Linux application paths.

The rebuilt implementation should preserve that behavior. Windows-specific
fallback paths are not required for v1, but PATH discovery may work on Windows if
the chosen browser automation dependency supports the host.

If no browser is found, fail with a clear hint:

```text
Chrome/Chromium was not found on PATH or in the standard local install locations checked by `ts audit`. Install a local Chrome or Chromium binary before running `ts audit`.
```

### 6.2 Browser session isolation

The browser session must be fresh and isolated:

- do not use the user's normal browser profile;
- do not reuse persistent cookies or local storage;
- do not load extensions;
- do not require interactive login;
- do not persist browser state after the command exits.

This keeps `ts audit` suitable for public-page onboarding and reduces the chance
of writing user-specific data into artifacts.

### 6.3 Navigation validation

The main document navigation is successful when the browser reports a status in
this range:

```text
200 <= status < 400
```

Redirects are allowed. The final URL after redirects is the canonical audited URL
for output artifacts and draft config generation.

Failures are fatal when:

- the browser does not report a main document response;
- the main request has browser failure text;
- the main response is missing;
- the main response status is outside `200..399`;
- browser launch, navigation, evaluation, or close fails.

Subresource failures do not fail the command in v1 because the old collector only
read resource timing data and did not capture reliable per-resource status codes.

### 6.4 Page settle heuristic

After navigation, wait for the page to settle using the old constants:

| Constant            | Value   |
| ------------------- | ------- |
| settle quiet period | `750ms` |
| poll interval       | `250ms` |
| max wait            | `6s`    |

At each poll:

1. Read `document.readyState`.
2. Read `performance.getEntriesByType('resource').length`.
3. If `readyState == "complete"` and the resource count has remained stable for
   the quiet period, the page is settled.

If the page does not settle before the max wait, continue with a warning instead
of failing:

```text
browser audit timed out while waiting for the page to settle; results may be partial
```

No CLI flags are defined for these timings in v1.

### 6.5 Collected page data

The collector produces this internal data model:

```rust
struct CollectedPage {
    requested_url: String,
    final_url: String,
    page_title: Option<String>,
    html: String,
    script_tags: Vec<CollectedScriptTag>,
    network_requests: Vec<CollectedRequest>,
    warnings: Vec<String>,
}

struct CollectedScriptTag {
    src: Option<String>,
    inline_text: Option<String>,
}

struct CollectedRequest {
    url: String,
    method: String,
    resource_type: Option<String>,
    status: Option<u16>,
}
```

For browser resource timing entries:

- `url` is the resource entry name;
- `method` is `GET` in v1;
- `resource_type` is the resource entry initiator type;
- `status` is `None` in v1.

The public `js-assets.toml` artifact must not include raw rendered HTML or raw
inline script text.

---

## 7. Analysis pipeline

The analyzer converts `CollectedPage` into an `AuditArtifact`.

Pipeline:

1. Parse `requested_url` and `final_url` as URLs.
2. Parse rendered HTML as a document.
3. Derive a title from the HTML `<title>` element.
4. Prefer the browser-reported title when it is non-empty; otherwise use the
   derived HTML title.
5. Start with collector warnings.
6. If requested URL and final URL differ, append a redirect warning.
7. Inspect script elements from rendered HTML.
8. Inspect browser-collected `document.scripts` entries.
9. Inspect resource timing entries whose type is `script`, case-insensitive.
10. Resolve, classify, detect, and deduplicate script assets.
11. Sort assets and integrations deterministically.
12. Count total JavaScript assets and third-party assets.

### 7.1 Script sources

Use three evidence sources because each catches different browser behavior:

| Source                       | Purpose                                                                     |
| ---------------------------- | --------------------------------------------------------------------------- |
| Rendered HTML `<script src>` | Captures scripts present in final DOM markup.                               |
| Browser `document.scripts`   | Captures normalized script URLs and inline scripts after runtime mutations. |
| Resource timing entries      | Captures dynamically loaded script network resources.                       |

For HTML `<script src>` values:

- resolve relative URLs against the final page URL;
- if resolution fails, append a warning and continue.

For browser `document.scripts` values:

- `src` should normally already be absolute;
- parse absolute script URLs;
- ignore malformed entries rather than failing the whole audit.

For resource timing entries:

- only entries with resource type `script`, case-insensitive, are treated as JS
  assets;
- parse absolute resource URLs;
- ignore malformed entries rather than failing the whole audit.

### 7.2 Deduplication and ordering

Assets are deduplicated by their absolute URL string after resolution/parsing.

Output order must be deterministic:

- assets sorted lexicographically by URL;
- detected integrations sorted lexicographically by integration ID;
- warnings preserved in append order.

If the same asset URL is observed from multiple sources, write one asset row. If
any source identifies the asset's integration, the final row should contain that
integration ID.

The last sentence is a slight tightening over the old implementation, which kept
the first inserted row unchanged. The user-visible intent is still the same:
produce one best-effort row per script URL.

### 7.3 Party classification

Each asset is classified as either:

```text
first-party
third-party
```

Classification is based only on the final page host and asset host.

An asset is first-party when any of these are true:

- `asset_host == page_host`;
- `asset_host` is a dot-boundary subdomain of `page_host`;
- `page_host` is a dot-boundary subdomain of `asset_host`.

Otherwise it is third-party.

This intentionally preserves the old lightweight host relationship heuristic. It
does not use the Public Suffix List and can be imperfect for complex delegated
subdomain setups. Those cases should be corrected manually by the operator when
reviewing `js-assets.toml`.

---

## 8. Integration detection

`ts audit` detects integrations from two evidence types:

1. script URL host/path patterns;
2. inline script text markers.

The initial detector set matches the old implementation:

| Integration ID       | URL evidence                          | Inline evidence                     | Draft config action                           |
| -------------------- | ------------------------------------- | ----------------------------------- | --------------------------------------------- |
| `google_tag_manager` | Known tag-manager script URL patterns | `GTM-...` container ID pattern      | Enable only when a container ID is extracted. |
| `gpt`                | Known GPT script URL patterns         | Case-insensitive `gpt` marker       | Enable `[integrations.gpt]`.                  |
| `didomi`             | Known Didomi script URL patterns      | Case-insensitive `didomi` marker    | Enable `[integrations.didomi]`.               |
| `datadome`           | Known DataDome script URL patterns    | Case-insensitive `datadome` marker  | Enable `[integrations.datadome]`.             |
| `permutive`          | Known Permutive script URL patterns   | Case-insensitive `permutive` marker | Add manual-review comment.                    |
| `lockr`              | Known Lockr script URL patterns       | Case-insensitive `lockr` marker     | Add manual-review comment.                    |
| `prebid`             | Known Prebid script URL patterns      | Case-insensitive `prebid` marker    | Add manual-review comment.                    |

The exact public vendor host/path substrings should be ported from the old
`feature/ts-cli` analyzer into runtime detector constants. Documentation and
examples should use `example` hostnames unless they are testing detector
constants directly.

### 8.1 URL detection

URL detection is best-effort and case-insensitive over the URL host and path.
Query strings can still be inspected separately for IDs such as a GTM container
ID.

When URL detection finds an integration:

- set `asset.integration` to that integration ID;
- add a `detected_integrations` entry if one does not already exist;
- use the script URL string as evidence.

### 8.2 Inline detection

Inline script text is inspected only for small integration markers. It is not
written to the audit artifact.

GTM container IDs are detected with the old shape:

```text
GTM-[A-Z0-9]+
```

Other v1 inline detectors are case-insensitive substring checks for the
integration IDs listed above.

When inline detection finds an integration:

- add a `detected_integrations` entry if one does not already exist;
- use the container ID as GTM evidence when available;
- otherwise use a concise marker such as `inline script matched <integration>`.

### 8.3 Evidence precedence

For each integration ID, keep the first evidence string encountered in the
analysis pipeline. Because output is sorted by integration ID, the evidence order
should not affect TOML ordering, only the evidence value.

### 8.4 Extensibility

The detector implementation should be a small data-driven table or a set of
focused helper functions so future integrations can be added without changing the
collector.

Do not add sourcepoint, APS, ad server, consent-string, ad slot, or bidder
configuration inference in v1 unless there is an explicit follow-up spec. The v1
requirement is to preserve the old detector set.

---

## 9. Audit artifact schema

`js-assets.toml` is the pretty TOML serialization of this schema:

```rust
struct AuditArtifact {
    audited_url: String,
    page_title: Option<String>,
    js_asset_count: usize,
    third_party_asset_count: usize,
    detected_integrations: Vec<DetectedIntegration>,
    assets: Vec<AuditedAsset>,
    warnings: Vec<String>,
}

struct DetectedIntegration {
    id: String,
    evidence: String,
}

struct AuditedAsset {
    kind: String,
    url: String,
    host: String,
    party: AssetParty,
    integration: Option<String>,
}

enum AssetParty {
    FirstParty,  // serialized as "first-party"
    ThirdParty, // serialized as "third-party"
}
```

Field rules:

- `audited_url` is the final URL after redirects.
- `page_title` is omitted when unknown.
- `js_asset_count` is `assets.len()`.
- `third_party_asset_count` counts assets whose `party` is `third-party`.
- `detected_integrations` contains one row per detected integration ID.
- `assets` contains one row per deduplicated script URL.
- `kind` is `script` for every v1 asset row.
- `host` is the asset URL host, or an empty string when unavailable.
- `integration` is omitted when no integration is detected for the asset.
- `warnings` is an array of human-readable warning strings.

No `schema_version` field is included in v1 so the artifact remains compatible
with the old `feature/ts-cli` shape. If a future schema version is added, it
should be additive and documented in a migration note.

Example shape using non-real hosts:

```toml
audited_url = "https://www.publisher.example/article"
page_title = "Example Publisher"
js_asset_count = 2
third_party_asset_count = 1
warnings = []

[[detected_integrations]]
id = "gpt"
evidence = "https://ads-vendor.example/tag/js/gpt.js"

[[detected_integrations]]
id = "google_tag_manager"
evidence = "GTM-ABC123"

[[assets]]
kind = "script"
url = "https://www.publisher.example/app.js"
host = "www.publisher.example"
party = "first-party"

[[assets]]
kind = "script"
url = "https://ads-vendor.example/tag/js/gpt.js"
host = "ads-vendor.example"
party = "third-party"
integration = "gpt"
```

---

## 10. Draft config generation

`trusted-server.toml` output is produced by taking the
`trusted-server.example.toml` starter template and applying audit-derived edits.

The generated file should preserve the starter template's comments and ordering
as much as possible. Text replacement is acceptable if the template has stable
sentinel values. A parsed-TOML implementation is also acceptable if it preserves
all required fields and produces a readable draft.

### 10.1 URL-derived fields

Use the final audited URL, not the originally requested URL.

From final URL:

| Config field              | Value                                        |
| ------------------------- | -------------------------------------------- |
| `publisher.domain`        | final URL host without port                  |
| `publisher.cookie_domain` | `.<host>`                                    |
| `publisher.origin_url`    | final URL origin, including non-default port |

Examples:

| Final URL                                 | `publisher.domain`      | `publisher.cookie_domain` | `publisher.origin_url`               |
| ----------------------------------------- | ----------------------- | ------------------------- | ------------------------------------ |
| `https://publisher.example/page`          | `publisher.example`     | `.publisher.example`      | `https://publisher.example`          |
| `https://www.publisher.example:8443/path` | `www.publisher.example` | `.www.publisher.example`  | `https://www.publisher.example:8443` |

If the final URL is missing a host, fail the audit as an internal audit error.
This should not happen after `<url>` validation for normal `http`/`https` URLs.

The command does not try to infer an apex cookie domain. Operators must review
and adjust `publisher.cookie_domain` for their domain policy.

### 10.2 Integration-derived edits

Detected integrations update only known starter-template sections.

| Detection                                 | Draft config edit                                                                   |
| ----------------------------------------- | ----------------------------------------------------------------------------------- |
| `gpt`                                     | Set `[integrations.gpt].enabled = true`.                                            |
| `didomi`                                  | Set `[integrations.didomi].enabled = true`.                                         |
| `datadome`                                | Set `[integrations.datadome].enabled = true`.                                       |
| `google_tag_manager` with container ID    | Set `[integrations.google_tag_manager].enabled = true` and `container_id = "<id>"`. |
| `google_tag_manager` without container ID | Do not enable automatically; add manual-review comment.                             |
| `permutive`                               | Add manual-review comment.                                                          |
| `lockr`                                   | Add manual-review comment.                                                          |
| `prebid`                                  | Add manual-review comment.                                                          |

Manual-review comments are appended near the end of the draft config:

```toml
# Audit findings requiring manual review
# - Detected prebid; review the corresponding [integrations.prebid] section before enabling it.
# - Detected permutive; review the corresponding [integrations.permutive] section before enabling it.
```

This preserves the old behavior for Permutive, Lockr, and Prebid while tightening
GTM handling: GTM should only be enabled when a usable container ID was actually
extracted.

### 10.3 What audit must not change

`ts audit` must not:

- remove starter-template sections;
- disable starter-template defaults;
- invent auction bidder settings;
- infer consent policy;
- infer request-signing settings;
- infer secret values;
- infer platform store names;
- add `[providers]` sections;
- write EdgeZero manifest fields;
- write environment overlays.

The command may only patch the fields listed in this section and append
manual-review comments.

---

## 11. Human output

On success, print a concise summary to stdout:

```text
Audited https://www.publisher.example/article
Title: Example Publisher
JS assets: 12
Third-party assets: 8
Detected integrations: google_tag_manager, gpt, prebid
Wrote: /path/to/js-assets.toml, /path/to/trusted-server.toml
```

Rules:

- `Audited` uses the final URL after redirects.
- `Title` uses `<unknown>` when no page title is found.
- `JS assets` is the artifact `js_asset_count`.
- `Third-party assets` is the artifact `third_party_asset_count`.
- `Detected integrations` is `none` when empty, otherwise comma-separated IDs in
  deterministic order.
- `Wrote` is `none` only if future command variants allow no files; in v1, both
  `--no-js-assets` and `--no-config` are rejected, so success should write at
  least one file.

Warnings are written into `js-assets.toml`. The success summary does not need to
print them unless a future UX pass adds a warning count.

---

## 12. Error behavior and exit codes

`ts audit` follows the base CLI exit code policy:

| Exit code | Meaning                                          |
| --------- | ------------------------------------------------ |
| `0`       | Audit completed and selected files were written. |
| `1`       | Audit failed.                                    |

No cancellation exit code is needed because `ts audit` has no interactive prompt.

### 12.1 Argument errors

| Failure                              | Message guidance                                |
| ------------------------------------ | ----------------------------------------------- |
| invalid URL                          | include the invalid value and parser error      |
| unsupported scheme                   | say `ts audit` only supports `http` and `https` |
| both outputs disabled                | say there is nothing to do                      |
| output path exists without `--force` | say refusing to overwrite and suggest `--force` |

### 12.2 Browser/audit errors

| Failure                    | Message guidance                                           |
| -------------------------- | ---------------------------------------------------------- |
| browser missing            | install Chrome or Chromium locally                         |
| browser launch failed      | include browser automation error context                   |
| navigation failed          | include requested URL context                              |
| main response missing      | say the browser did not capture the main document response |
| main status not `200..399` | include status, status text, and response URL              |
| page evaluation failed     | include which data collection step failed                  |
| browser close failed       | report close failure; do not silently ignore it            |

### 12.3 Non-fatal warnings

Warnings do not cause a non-zero exit:

- page settle timeout;
- requested URL redirected to final URL;
- individual malformed script URLs in rendered HTML.

Warnings are stored in the artifact and should be visible during review.

---

## 13. Security and privacy notes

`ts audit` intentionally loads a real public web page and allows that page's
scripts to execute in headless Chromium. Treat it as an operator-controlled
onboarding tool, not an unattended crawler.

Required safeguards:

- use an isolated temporary browser profile;
- do not use the operator's personal browser profile;
- do not persist cookies or storage;
- do not write raw inline script bodies;
- do not write rendered HTML;
- do not write request or response bodies;
- do not write browser cookies, local storage, session storage, or form values;
- do not print generated config values that may contain secrets;
- do not contact any platform APIs;
- do not upload artifacts anywhere.

The artifact still contains URL inventory. Operators should treat it as
potentially sensitive publisher/vendor information.

Docs, tests, and committed fixtures should use `example` domains and fictional
publisher data. Runtime detector constants may contain the public vendor
host/path patterns required for actual detection.

---

## 14. Implementation architecture

The base CLI spec owns the crate and binary. `ts audit` should be implemented as
an internal module of the host-target CLI crate, not as part of
`trusted-server-core` or any wasm-target adapter crate.

Suggested module layout:

```text
crates/trusted-server-cli/src/
  audit.rs              # public command orchestration and output writing
  audit/
    collector.rs        # collected data structs and collector trait
    browser_collector.rs# Chrome/Chromium implementation
    analyzer.rs         # artifact analysis and integration detection
```

### 14.1 Host-only dependencies

Host-only browser dependencies are allowed in `trusted-server-cli` only.

Do not add browser automation, Tokio runtime, `which`, or scraper dependencies to
runtime crates that build for `wasm32-wasip1`.

The prior implementation used:

- `chromiumoxide` for browser automation;
- `tokio` current-thread runtime inside the sync command handler;
- `scraper` for rendered HTML parsing;
- `regex` for GTM ID detection;
- `url` for URL parsing/resolution;
- `toml`/`serde` for artifact serialization.

Reusing those dependencies is acceptable. Replacing `chromiumoxide` is also
acceptable if the command preserves the same collection behavior and tests can
run without a real browser.

### 14.2 Testability boundary

Use a collector abstraction so unit tests can feed synthetic `CollectedPage`
values directly into the analyzer and draft-config generator.

Unit tests must not require a real browser. Browser smoke tests, if added, should
be ignored by default or feature-gated.

Recommended orchestration shape:

```rust
trait AuditCollector {
    fn collect_page(&self, target_url: &Url) -> Result<CollectedPage, Report<CliError>>;
}

fn perform_audit_with_collector(
    collector: &dyn AuditCollector,
    target_url: &Url,
) -> Result<AuditOutputs, Report<CliError>>;
```

The production command uses the browser collector. Tests use a fake collector.

### 14.3 Output preflight and writes

Build an `AuditOutputPlan` before launching the browser:

```rust
struct AuditOutputPlan {
    js_assets_path: Option<PathBuf>,
    config_path: Option<PathBuf>,
    force: bool,
}
```

The plan should:

- reject both paths disabled;
- resolve defaults;
- check overwrite conflicts for all selected paths;
- avoid writing anything until all output content is ready.

Atomic file replacement is not required in v1, but avoiding partial writes from
known path conflicts is required.

---

## 15. Tests

### 15.1 CLI arguments and path planning

- `ts audit <http-url>` parses.
- `ts audit <https-url>` parses.
- non-HTTP schemes are rejected.
- malformed URLs are rejected.
- both `--no-js-assets` and `--no-config` are rejected.
- default paths resolve to `js-assets.toml` and `trusted-server.toml` under the
  current working directory.
- custom `--js-assets` and `--config` paths are honored.
- parent directories are created for selected outputs.
- existing outputs are rejected without `--force`.
- existing outputs are overwritten with `--force`.
- if one selected output exists and another does not, no file is written before
  the command reports the overwrite conflict.

### 15.2 Browser collector units

- browser executable discovery finds PATH candidates.
- browser executable discovery checks supported fallback paths.
- missing browser produces the install hint.
- navigation statuses `200`, `302`, and `399` are accepted.
- navigation statuses below `200` and at or above `400` are rejected.
- missing main document response is rejected.
- failed main document request is rejected.
- settle timeout returns a warning, not an error.

Browser-launch integration tests should be opt-in and skipped by default.

### 15.3 Analyzer

- rendered HTML `<title>` is used when browser title is absent.
- browser title wins over rendered HTML title when present.
- requested-to-final URL redirect adds a warning.
- relative script URLs resolve against the final URL.
- malformed script URLs add warnings and do not fail the audit.
- HTML script tags, browser script tags, and script resource timing entries are
  merged.
- duplicate script URLs produce one asset row.
- duplicate script URLs preserve detected integration when any source detects it.
- assets are sorted by URL.
- integrations are sorted by ID.
- first-party exact host match is classified as `first-party`.
- first-party subdomain relationship is classified as `first-party`.
- unrelated host is classified as `third-party`.
- non-script resource timing entries are ignored.

### 15.4 Integration detection

- GTM container IDs are extracted from inline script evidence.
- GTM container IDs are extracted from script URLs when present.
- GPT URL evidence detects `gpt`.
- Didomi URL evidence detects `didomi`.
- DataDome URL evidence detects `datadome`.
- Permutive URL evidence detects `permutive`.
- Lockr URL evidence detects `lockr`.
- Prebid URL evidence detects `prebid`.
- inline markers detect `gpt`, `didomi`, `datadome`, `permutive`, `lockr`, and
  `prebid` case-insensitively.
- detector tests should avoid real publisher domains.

### 15.5 Artifact serialization

- `js-assets.toml` includes `audited_url`, counts, integrations, assets, and
  warnings.
- `page_title` is omitted when unknown.
- `asset.integration` is omitted when unknown.
- `party` serializes as `first-party` or `third-party`.
- `js_asset_count` equals the number of asset rows.
- `third_party_asset_count` equals the number of third-party asset rows.
- output is deterministic for the same collected input.

### 15.6 Draft config generation

- final redirected URL is used for config fields.
- `publisher.domain` uses final host without port.
- `publisher.cookie_domain` uses `.<host>`.
- `publisher.origin_url` preserves non-default port.
- GPT detection enables `[integrations.gpt]`.
- Didomi detection enables `[integrations.didomi]`.
- DataDome detection enables `[integrations.datadome]`.
- GTM detection with container ID enables Google Tag Manager and sets
  `container_id`.
- GTM detection without container ID does not enable Google Tag Manager and adds
  a manual-review comment.
- Permutive, Lockr, and Prebid detections add manual-review comments.
- no platform/provider/EdgeZero sections are added.
- generated TOML parses successfully.

### 15.7 Command orchestration

Using a fake collector:

- selected outputs are written on success.
- `--no-js-assets` writes only config.
- `--no-config` writes only assets.
- stdout summary includes final URL, title, counts, integrations, and paths.
- collector warnings are present in `js-assets.toml`.
- no EdgeZero delegate is invoked.
- no platform API is contacted.

---

## 16. Implementation plan

### Stage 1 — Wire command into the base CLI

- Add `AuditArgs` to the new `ts` command tree.
- Add `Command::Audit(AuditArgs)` dispatch.
- Reuse base CLI output and error formatting helpers.
- Add URL parsing/validation.
- Add output-path planning and overwrite preflight.

### Stage 2 — Port audit data model and analyzer

- Add `CollectedPage`, `CollectedScriptTag`, and `CollectedRequest`.
- Add `AuditArtifact`, `AuditedAsset`, `DetectedIntegration`, and `AssetParty`.
- Port analyzer behavior from `feature/ts-cli`.
- Port integration detector behavior from `feature/ts-cli`.
- Add deterministic sorting/deduplication.
- Add analyzer and artifact serialization tests.

### Stage 3 — Port draft config generation

- Read from the new `trusted-server.example.toml` template.
- Patch URL-derived publisher fields from the final URL.
- Enable only the supported auto-enable integrations.
- Append manual-review comments for inferred-only integrations.
- Keep generated config as a draft.
- Add draft config tests.

### Stage 4 — Port browser collector

- Add host-only browser automation dependencies to `trusted-server-cli`.
- Implement browser discovery.
- Implement fresh headless browser launch.
- Implement navigation validation and settle wait.
- Collect final URL, title, rendered HTML, script tags, and resource timing
  entries.
- Ensure browser close errors are surfaced.
- Add unit tests around browser-independent helper functions.

### Stage 5 — Docs and verification

- Document `ts audit` in the CLI guide.
- Document Chrome/Chromium requirement.
- Document generated files and draft-config caveat.
- Add default generated artifact path to `.gitignore` if not already ignored.
- Run host-target CLI tests.
- Run workspace formatting and linting required by the base CLI implementation
  work.

---

## 17. Open follow-ups outside this spec

- Add `--browser <path>` or `TS_AUDIT_BROWSER` override for non-standard Chrome
  installs.
- Add `--timeout` or named audit profiles for slow pages.
- Add `--json` machine-readable command summary.
- Add schema versioning for `js-assets.toml`.
- Add Sourcepoint, APS, ad server, or additional integration detectors.
- Add optional HAR export with explicit user opt-in.
- Add authenticated audit support using an explicitly provided isolated browser
  profile.
- Add multi-page crawl support.
- Add richer config bootstrap for consent, auction, bidder, and proxy settings.
- Add a command to merge audit findings into an existing config instead of
  writing a fresh draft.
