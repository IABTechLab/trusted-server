# Documentation Refresh (Full Surface)

**Date:** 2026-08-19
**Revised:** 2026-08-27 (round 8; re-audited and regenerated against the moved release branch)
**Status:** Draft, pending review
**Scope:** Documentation and doc tooling. No runtime behavior changes.
**Baseline:** `rc/202608` at merge base `a163367b3` (2026-08-27, the main→rc
merge that landed #870). Every inventory in this spec was regenerated from
that commit by a fresh four-track delta audit. Baseline contract: before
implementation starts, and after any rebase, assert that the branch's merge
base with `origin/rc/202608` equals the SHA recorded here; if it does not,
re-run the delta audit and update this spec first. Rounds 1-7 of review
history live in git; this revision supersedes their inventories.

## Context

Trusted Server's documentation spans the VitePress site (`docs/`), root and
per-crate markdown, in-code documentation (rustdoc, clap help, JSDoc), and
configuration templates. The original audit found systemic drift; the
release branch has since fixed a meaningful subset itself and changed the
runtime model underneath the rest. Current state at the baseline:

**Already fixed on rc (removed from this spec's worklist):**

- The fabricated `GET /first-party/ad` / `POST /third-party/ad` endpoints
  are gone from all four pages that carried them; `api-reference.md` now
  documents `POST /auction` and the `/_ts/admin/ec`, `/_ts/admin/ec/{id}`,
  and `/_ts/admin/eids` diagnostics with an auth-coverage contract.
- `format.yml` now runs `vitepress build` on PRs (dead internal links fail
  CI). Note the interaction: with no `srcExclude`, CI now builds all 127
  internal `docs/superpowers/**` files as site pages.
- `cli.md` covers `active-version`, `healthcheck`, `rollback`, and
  `config gc`; `configuration.md` documents the secret-store migration
  (all 11 secret paths), `[trusted_client_ip]`, and the config-first
  `[auction.providers.<id>]` model; `getting-started.md` was rewritten
  around the blob + secret-reference flow.
- Spin no longer hardcodes the example config: startup reads the blob from
  Spin's `default` KV store and resolves secret references through Spin
  variables. The old blocking follow-up is closed.

**Still open (verified at the baseline):**

1. **Publishing and policy hygiene.** No `srcExclude` in
   `docs/.vitepress/config.mts` (127 internal spec/plan files build into
   the public site); `docs/guide/index.md` is 0 bytes; the nav Guide link
   targets `/guide/getting-started` and a Business Value nav item points at
   `business-use-cases.md` (uncited quantitative claims; presents planned
   headless-browser malvertising detection as shipped while `roadmap.md`
   calls it planned); `docs/public/CNAME` is the literal
   `your-custom-domain.com`; `fastly.toml` carries a real personal email
   (line 4), the real service id (line 10), the orphaned
   `test-prebid-eids.sh` comment (line 38), and inconsistently labeled key
   fixtures; `docs/package.json` is ISC and not `private`;
   `docs/guide/onboarding.md` publishes internal contacts and access
   guidance.
2. **Fabricated or dead content that survives.**
   `architecture.md:93-104` still shows the nonexistent `RequestWrapper`
   trait (also in `.claude/agents/code-architect.md:16`); `ad-serving.md:11`
   still documents Equativ (also `.claude/agents/issue-creator.md:85` and
   `FAQ_POC.md`; it is gone from `integration-guide.md`);
   `.with_asset(...)` remains in `creative-processing.md:808` and
   `integration-guide.md:84,248`; `error-reference.md:614` still says
   `npm run type-check`; `configuration.md:2287` still imports the
   nonexistent `settings_data::get_settings`; the auction README's rotted
   route table, `providers/` directory, and APS `mock` sections;
   `onboarding.md`'s dead `SEQUENCE.md` links; `TESTING.md` is still the
   auction curl runbook; `FAQ_POC.md` is still false on every axis.
3. **References behind the new runtime model.** `Settings` now has 17 root
   fields (new `Option<TrustedClientIpConfig>`; `request_signing` and
   `creative_opportunities` are also `Option`), but `configuration.md`
   still has no `[consent]` or `[debug]` reference sections, documents
   `[tinybird]` only inside Quick Start, and its "Key Sections" table
   lists 10 of 17 roots. Docs claim reserved-field protection for both
   `request_ext` and `imp_ext` while `reject_reserved_fields` guards only
   `request_ext`. `adserver_mock` is doubly stranded: rc deleted its old
   config subsection without a replacement page. The CHANGELOG carries 8
   breaking `[Unreleased]` entries with inconsistent `**Breaking**`
   formatting and two dead `v1.2.0` compare links (no tag exists).
4. **Adapter truth gaps.** `/health` is not registered on Cloudflare;
   `/_ts/admin/eids` is a real handler on all four adapters while
   `/_ts/admin/ec{,/{id}}` are registered everywhere but functional only
   on Fastly (KV-backed) and key rotation returns 501 off Fastly;
   Cloudflare and Spin reject multi-provider auction plans at startup
   (capability `concurrent_provider_fanout = false`; dormant configs are
   accepted); startup-failure behavior differs (Spin: hardened 503 router
   that keeps `/health` alive; Cloudflare/Fastly: 500; Cloudflare/Axum
   degraded routers answer errors on every path); `Hooks::stores()` is
   implemented only by Fastly, so on Cloudflare and Spin the request-time
   config/KV registries are empty - the declared `TRUSTED_SERVER_KV`
   binding is never opened, Spin's `v_current_x2dkid`/`v_active_x2dkids`
   variables are unreachable, the Cloudflare `platform.rs:579-592` rustdoc
   describing injected handles is false, and `cloudflare.toml` is dead
   config referenced by nothing live. A bare `fastly compute serve` from
   the checked-in `fastly.toml` cannot start the app: the
   `trusted_server_config` store is empty and `ts_secrets` lacks the three
   required keys, so every non-health path returns 500.
5. **Missing coverage.** No pages exist for Cloudflare, Spin, or Axum
   deployment, EdgeZero, telemetry/Tinybird, tsjs, GPT slot handoff,
   script guards, parity testing, or `adserver_mock`; seven of ten crates
   have no README; the integration guide's snippets still do not compile
   (`RuntimeServices` omissions, `fastly::http` import in core-neutral
   code); `integrations-overview.md` covers 7 of 14 IDs.
6. **No enforcement beyond the new docs build.** No `cargo doc` in CI,
   doctests never run (cross-compile only), no parity between code and the
   hand-maintained inventories, `eslint-plugin-jsdoc` inert,
   `openrtb-codegen` missing `[lints] workspace = true`, the PR template
   still says `tracing`, and the slash-command files omit Spin/parity
   gates.

Appendices A-E carry the regenerated inventories with citations.

## Decision

Treat documentation as a product surface with a defined source of truth per
artifact, fix the verified-open findings in eight work packages, and add
enforcement, including executable parity checks bound to the reader-facing
markdown, so drift is caught by CI instead of by the next manual audit.
Every claim in the refreshed docs must be verifiable against code at the PR
HEAD's merge base with `rc/202608`; anything aspirational is labeled or
removed; adapter support claims come from an owned support matrix grounded
in current operational evidence.

The source-of-truth map:

| Artifact                   | Truth source                                                                                                                                                                                           | Consumers               |
| -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------- |
| HTTP API reference         | Adapter route tables and entry points (`adapter-*/src/{app,main,lib,platform}.rs`) + core handlers                                                                                                     | Publishers, partners    |
| Config reference           | `Settings` (17 roots, `deny_unknown_fields`) + the typed per-integration configs + the provider profile schemas (`PROFILE_REGISTRATIONS`) + `secret_fields()`                                          | Operators               |
| CLI reference              | The built `ts` binary's recursive `--help` tree (Linux + macOS)                                                                                                                                        | Operators               |
| Integration pages          | The named inventories in Appendix C: deploy IDs (14), registry `builders()` (11), plan registrations (prebid, aps), profile registry (3), mediator (`adserver_mock`), JS modules (12) and bundles (13) | Publishers, integrators |
| Integration guide snippets | A compiling sample integration (`testlight` or a doc-tested fixture)                                                                                                                                   | Integrators             |
| Deployment guides          | Adapter manifests + per-adapter startup paths (Appendix D) + support matrix                                                                                                                            | Operators               |
| Architecture               | `Cargo.toml` workspace members + `core/src/platform/` + `AuctionPlan`                                                                                                                                  | Contributors            |
| Test/CI docs               | `.cargo/config.toml` aliases + `.github/workflows/*`                                                                                                                                                   | Contributors            |

## Source sets

Truth-pass acceptance and parity checks operate on defined source sets:

- **Active public set:** everything VitePress builds (`docs/**` minus the
  WP1 `srcExclude` list).
- **Active repo set:** root markdown (`README.md`, `CONTRIBUTING.md`,
  `TESTING.md`, `CHANGELOG.md`, `FAQ_POC.md` until actually retired,
  `ProjectGovernance.md`, `AGENTS.md`, `CLAUDE.md`), crate READMEs, config
  templates (`trusted-server.example.toml`, `fastly.toml`, `edgezero.toml`,
  `.env.example`, `.env.dev`), and `.claude/commands/*.md`.
- **Active maintained internal set:** `docs/README.md`, `docs/internal/**`
  (including the moved onboarding page), `docs/epics/**` (maintained
  internal records), `docs/business-use-cases.md` while excluded-but-
  tracked, `scripts/README.md`, `tinybird/README.md`, and
  `tools/docs-parity/README.md` once created, the human-facing comments of
  `.github/workflows/**`, `.github/actions/**`, and `scripts/*.sh` usage
  headers, the comment surfaces of the adapter manifests (`fastly.toml`,
  `wrangler.toml`, `wrangler.ci.toml`, `spin.toml`, `axum.toml`, and
  `cloudflare.toml` until retired), `.claude/skills/**`,
  `.claude/agents/**`, and `.github/pull_request_template.md`. A checked
  maintained-source manifest enumerates entries as `{path, mode,
selector}` (whole-file vs comment-region), and the WP8b inventory gate
  asserts final set equality against it.
- **Historical set:** `docs/superpowers/**` and shipped `CHANGELOG.md`
  release entries. Exempt from retired-term greps only; privacy/secret
  scanning covers ALL tracked files (see WP2).

## Goals

- Every endpoint, config key, command, flag, crate name, and code path
  named in active-set documentation exists at the baseline, with
  per-adapter availability stated where behavior differs.
- Every shipped operator- or publisher-visible surface is documented: all
  14 deploy-validated integration IDs, all 17 config roots, the provider
  profile model, the adapters (with evidence-based maturity labels), all
  `ts` commands, telemetry, and tsjs.
- The adapter support model is truthful and mechanically canonical: a
  checked adapter-support record renders the matrix and every repeated
  status summary.
- The public site publishes only intended pages, containment reaches the
  live site (Pages deploys only from `main`), and internal details are
  scrubbed from the public repository regardless of build exclusion.
- Sensitive real-world values are removed or covered by the typed,
  expiring allowlist (the `fastly.toml` `service_id` is its one
  owner-approved, time-bounded entry pending the ops-owned migration).
- CI catches regressions: docs build (already live on rc), rustdoc with
  broken-intra-doc-link denial, doctests, and semantic parity bound to the
  reader-facing markdown.

## Non-goals

- No runtime changes. Code defects found by the audits are follow-ups
  (list below), not in-scope work; parity checks add tests/tools only.
- No new documentation toolchains beyond the `tools/docs-parity` dev tool.
- No rewrite of `business-use-cases.md` marketing copy: it is excluded
  from the build via `srcExclude` and carries a source-level unverified
  banner (WP1) until an evidence-based rewrite happens (open question 4).
  `roadmap.md` gets a factual status pass only.
- No release management. The 8 breaking `[Unreleased]` entries are a
  maintainer decision; the deterministic no-release CHANGELOG edit is:
  normalize the `**Breaking**` marker formatting, keep `[1.2.0]` with a
  "(tag v1.2.0 was never published)" annotation, remove its dead link
  reference, repoint `[Unreleased]` to `v1.1.0...HEAD`. If a release lands
  before merge, rebase and re-audit.
- No accessibility audit gate (stock VitePress theme); WP5 ships diagram
  prose equivalents, local search, and `lastUpdated`.
- Not chasing 100% rustdoc coverage. "Full surface" for in-code docs means
  the WP7 worklist plus the known-false rustdoc repairs (e.g. the
  Cloudflare `platform.rs` stores claim), not every item.

## Delivery shape

Owner-directed single rc PR (#1049) carrying the spec plus all packages,
one reviewable commit (or small series) per package, with package-level
review checkpoints (acceptance evidence recorded before the next package
lands), generated-output changes in their own commits, and no squash on
merge. One exception forced by mechanics: GitHub Pages deploys only on
pushes to `main`, so the containment subset also ships as a minimal
separate PR straight to `main`: exactly the `srcExclude` change (covering
`superpowers/**`, `internal/**`, `epics/**`, `guide/onboarding.md`,
`README.md`, `business-use-cases.md`), the onboarding move/scrub, the
filled Guide landing page, and the nav edits that removing pages forces
(Guide link to the landing page; Business Value item dropped). Removing
every link to an excluded source is a containment invariant. Neither the
CNAME resolution nor the marketing-page disposition blocks or rides in it.
CodeQL gets `rc/*` PR triggers (WP8b) and joins the final gate list.
Rollback treats exclusions and scrubbing as non-rollbackable security
invariants: recovery reverts only the causal non-security commit or
redeploys a known-good artifact that retains them. Open question 7 asks
the owner to confirm this shape.

## Work packages

### WP1: Publishing and policy hygiene

- `srcExclude` + onboarding move/scrub + Guide landing page + nav edits
  (contents fixed in Delivery shape); the containment PR to `main` ships
  first and its post-deploy smoke asserts: excluded URLs 404; site root,
  the Guide landing page, and one reference page return 200 with expected
  content.
- `business-use-cases.md` gains its source-level unverified banner
  (asserted by WP1 acceptance).
- Resolve `docs/public/CNAME` in its own follow-up commit (open question
  2; both branches specified: delete and re-smoke project URLs, or custom
  domain with `base: '/'`, Pages/DNS/TLS setup, canonical+asset smokes,
  and a project-owned-public-domain allowlist classification).
- `fastly.toml`: empty `authors` list; label the key fixtures
  consistently as local test fixtures; comment the four KV stores; remove
  the `test-prebid-eids.sh` comment. `service_id` stays under its
  allowlist entry pending the ops migration (open question 1).
- `docs/package.json`: `"private": true`, license Apache-2.0.
- `.github/pull_request_template.md`: `tracing` → `log`; test-plan gates
  become a link to the canonical gate region (link-only mode).
- `.claude/commands/*.md` converted to link-only gate references;
  `AGENTS.md` gets a generated gate region (it is the fallback for agents
  that cannot read `CLAUDE.md`, so it carries the list).

Acceptance: `vitepress build` output contains none of the excluded pages;
the `main` containment PR is merged and its smoke passes; the banner is
present; no internal contacts or access instructions anywhere in the repo;
every command file links to (not copies) the canonical gates.

### WP2: Truth pass over existing content

Disposition-based: every document in the active public and maintained
internal sets gets verified / rewrite / retire / created recorded in a
checked inventory under `docs/internal/audits/`, stamped with the audited
merge-base SHA, with source anchors; non-page surfaces get region-level
dispositions. The inventory is an audit record; the WP8 gates are the
continuing control. Executable fences are governed by the WP8a snippet
manifest (all languages, graded modes, expiring waivers).

Content items (all verified open at the baseline):

- Remove `RequestWrapper` from `architecture.md` (replace with the real
  platform traits) and from `.claude/agents/code-architect.md`; remove
  Equativ from `ad-serving.md` (rewrite the page around the real flow),
  `.claude/agents/issue-creator.md`, and via the `FAQ_POC.md` handling
  below; remove `.with_asset(...)` (closes #277); fix `npm run
type-check` and the `settings_data::get_settings` example (the exported
  loader path); retired-token cleanup for maintained agent files happens
  here so WP2's own checkpoint grep can pass.
- Reserved-field truth fix: docs claim `request_ext` and `imp_ext`
  protection; code guards only `request_ext`. Fix the docs to match code
  and file the follow-up asking whether `imp_ext` should be guarded.
- Auction README repairs (route table by symbol name, real provider
  layout: `AuctionPlan`, `PROFILE_REGISTRATIONS`, `GenericOpenRtbProvider`,
  mediator; remove the removed-`mock` sections).
- Retire `FAQ_POC.md` (fallback if rejected: archive or factual rewrite);
  replace `gam.md`/`kargo.md` with tombstone content (routes preserved
  unconditionally, `tombstone` orphan-allowlist kind, old-route smokes).
- `TESTING.md` rewritten as the test-matrix index; the auction runbook
  verified-then-rewritten into `docs/guide/auction-testing.md`.
- CHANGELOG: the deterministic no-release edit (Non-goals) plus the
  missing operator-visible entries check.
- Environment files: `.env.example`/`.env.dev` retired-key cleanup and the
  two-surface model (runtime variables vs `ts config` overlay).
- Fictional-data pass with the typed allowlist (vendor URL / hash-pinned
  fixture / historical / service ID / project-owned public domain), owner
  - rationale + expiry per entry; scanning covers ALL tracked files
    (negative fixtures are synthesized at test time, never checked in);
    remediation of non-document fixtures (e.g. the scraped
    `html_processor.test.html`) is owned here with regression tests re-run;
    rotation/history-rewrite decisions escalate per finding.
- Human-facing workflow comment repairs: the Spin release-build comment
  (claims env overrides fix embedded settings; startup now reads the KV
  store) and the `test-cli` comment (claims a workspace default target
  that `.cargo/config.toml` does not set).
- Roadmap status pass (shipped/active/deferred; also reconcile the
  malvertising-detection claim with `business-use-cases.md`).

Acceptance: checked-in inventory complete; greps over the active sets
(historical exempt) for `RequestWrapper`, `equativ`, `with_asset`,
`type-check`, `settings_data::get_settings`, `SEQUENCE.md`,
`synthetic_id` (outside shipped changelog entries), and `mock = true` (APS
context) return nothing; the all-tracked privacy scan is clean modulo the
allowlist; checkpoint scope = surfaces this package touches, full-set
greps re-run at final HEAD.

### WP3: Configuration reference completion

Parity with the 17-field `Settings`, the typed integration configs, the
provider profile schemas, and the secret model.

- `configuration.md`: add the missing `[consent]` and `[debug]` reference
  sections; promote `[tinybird]` from Quick Start prose to a reference
  section; complete the "Key Sections" table to all 17 roots; extend
  Integration Configurations from 5 to all 14 IDs (audit the existing
  five against the field inventory); generate the
  `[auction.providers.<id>.profile_config]` reference from the three
  typed profile schemas (each profile's fields, defaults, timeout
  defaults, byte/depth/key limits, endpoint canonicalization, reserved
  fields).
- `trusted-server.example.toml`: add the missing commented blocks
  (`[consent]`, `[rewrite]`, `[tester_cookie]`, `[image_optimizer]`,
  `[[proxy.asset_routes]]`, osano; extend `[tinybird]` if incomplete),
  preserving the template's contract: placeholder strings must stay in
  the rejection constants' exact forms (three test call sites splice on
  the literal strings and the `# [header]` comment style), and
  secret-reference fields carry key names, never values.
- Secret-model documentation: classify every `Redacted` path as
  store-resolved (the 11 `secret_fields()` entries) or deliberately
  inline (`trusted_client_ip.shared_secret`,
  `tinybird.access_token_secret`) with explicit exposure guidance; the
  migration section already on rc gets a verified disposition. CLI/config
  pages warn that `ts config diff`/`--dry-run`/push output can print
  inline-secret values.
- The extractor-based field inventory (WP8a) carries semantics: resolved
  defaults via the literal-AST + companion-manifest + compiled-probe
  chain; requiredness; grammars; ranges from `#[validate]`; every custom
  validator in Appendix B's inventory gets a companion entry with
  positive/negative probes, fail-closed on unclassified validator
  functions; `serde(skip)` fields never become documented paths;
  canonical keys vs deprecated aliases (`pub_id`, `s3_sig_v4`).

Acceptance: every inventory field path appears in template and reference;
every deploy-validated ID has a config subsection matching its struct;
the WP8a harness passes; the parity checklist (Appendix B) is in the PR
description.

### WP4: API reference completion

rc already rebuilt much of the reference; this package brings it to the
contract standard and binds it to generated regions.

- Route/availability tables become generated regions from the checked
  route inventory (Appendix A), which records per-adapter availability
  (Cloudflare lacks `/health`; `/_ts/admin/eids` real on all four;
  `/_ts/admin/ec{,/{id}}` registered everywhere, functional only on
  Fastly; key rotation 501 off Fastly; EC partner API, tester cookies,
  and JA4 debug Fastly-only), method shapes (page-bids GET plus
  denied-OPTIONS; sign/proxy-rebuild GET+POST; identify GET+OPTIONS),
  the seven-method publisher fallback, and route families (literal /
  template / config-derived / conditional with config source or
  predicate - Prebid `script_patterns`, prefix overrides, APS renderer
  route only in `trusted_server` rendering mode).
- Per-endpoint contract checklist for manually owned prose (auth,
  schemas, response codes, cache/CORS, config gates, rate limits,
  examples), with explicit ownership markers; `/first-party/sign` (mints)
  vs `/proxy`/`/click`/`/proxy-rebuild` (validate) stay distinguished.
- Startup-failure behavior documented per adapter (Spin hardened 503 with
  live `/health`; Cloudflare/Fastly 500; degraded-router differences).
- `trusted_client_ip` documented as middleware (sanitization on all
  adapters, IP resolution only on Fastly), not a route.
- Cloudflare route parity source: `docs-parity` parses the `build_router`
  chain with a fail-closed grammar that expands the known constants and
  loops (path arrays, `publisher_fallback_methods()`); an unrecognized
  construct fails the check rather than undercounting.

Acceptance: generated regions match the checked inventory per adapter;
contract checklist satisfied; manual-ownership markers present.

### WP5: New coverage pages and navigation

- Deployment guides grounded in the audited startup paths, each with a
  first-success smoke that provisions BOTH halves (config store and
  secret store) from clean state:
  - `fastly.md` additions + quick start: init/validate →
    `ts config push --adapter fastly --local` → seed the three required
    `ts_secrets` keys → `fastly compute serve` → `/health` → publisher
    request against a stub origin → restore the mutated `fastly.toml`.
  - `cloudflare.md`: the nested `{"app_config": "<envelope>"}` wrapper,
    the documented envelope-transfer bridge (push writes KV that the
    Worker never reads - warn that a green push does not configure the
    Worker), `wrangler secret put` for the required key names, the
    `wrangler.ci.toml`/generated-manifest pattern, no `/health`
    (readiness via another route), the single-provider restriction, and
    the unwired stores (`TRUSTED_SERVER_KV` never opened) per the
    follow-up.
  - `spin.md` (now writable - the runtime fix landed): `ts config push
--adapter spin --local` with the required
    `EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME=default`
    mapping (push writes SQLite under `.spin/`; runtime reads store label
    `default` - the mismatch footgun is documented), Spin variable names
    generated from the operator's key names via the encoder (empty
    defaults fail closed), `spin up`, a non-health request, cleanup.
    Maturity label from current evidence: experimental - no
    integration-test environment, single-provider only, request-time
    config/KV stores unwired.
  - `axum-dev.md`: local development only; the env-var config/secret
    bridge with exact variable names; read-only admin EIDs available,
    key rotation and EC KV lookups not.
- Support matrix from a checked adapter-support record (generated regions
  everywhere adapter status is stated), including provider fan-out
  capability and startup-failure behavior columns.
- `edgezero.md` (manifest, stores, blob flow, lifecycle commands),
  `telemetry.md` (+ `tinybird/README.md`; emission Fastly-only),
  `tsjs.md` (module system including the third loading mode:
  `gpt_diagnostics` standalone tag; 12 modules / 13 bundles),
  `integrations/adserver_mock.md` (as the mediator, with
  `auction.mediator` context - its old config subsection was deleted on
  rc), GPT slot handoff in `gpt.md`, script guards in the integration
  guide (closes #341), `testlight` reference section, compiling snippet
  source for the integration guide.
- `integrations-overview.md` extended to 14 IDs + the creative row from
  the capability record.
- Navigation restructure (Operator/Deployment/Reference groups), local
  search, `lastUpdated`, rolling-main banner with build-SHA provenance
  (`GITHUB_SHA` injected; smoke asserts it), diagram prose equivalents
  with a checked inventory, journey walks in acceptance.
- CLI reference from the two-platform help union; description text passes
  the internal-term gate with the expiring override table.

Acceptance: every ID documented and nav-reachable; every adapter guide
consistent with the support record; snippets compile; `vitepress build`
green; search/banner/diagram/journey assertions recorded; each adapter
smoke's exact commands and cleanup recorded in the PR description.

### WP6: Root markdown and crate READMEs

- Audit every existing root/crate/skill/agent document (deep audit here;
  WP2 already removed falsehoods): `README.md` quick starts must satisfy
  the first-success contracts; `CONTRIBUTING.md` refresh (link-only gate
  reference); `CLAUDE.md` corrections (no workspace default target; the
  integration-system section predates the plan model; `# Examples`
  standard reconciled to the earn-their-keep rule; vendor-endpoint
  exception sentence); integration-tests README fixes; governance doc
  intent statements.
- New READMEs for the seven crates lacking one, plus `scripts/README.md`;
  core README rewritten as a real overview; `readme =` keys in Cargo.toml.

Acceptance: every `cargo metadata` package has a README; dispositions
recorded; quick-start journeys proven.

### WP7: In-code documentation

Worklist (acceptance scope): core `lib.rs` module index (12 of 37 listed
today); `platform/` docs (4 of 10 files documented on rc; test-only
excluded); crate-level headers for fastly/cloudflare/js/cli; module docs
for the undocumented core files (settings, settings_data, http_util,
proxy, auth, tsjs, openrtb, price_bucket, rsc_flight, host_rewrite,
storage, html_processor, registry, prebid, nextjs/, datadome/);
`constants.rs` items; CLI module docs; the TypeScript files (headers +
complete `core/types.ts`), `build-prebid-external.mjs` header. Plus the
known-false rustdoc repairs: the Cloudflare `platform.rs:579-592` stores
claim (and its Spin sibling) rewritten to match the unwired reality,
citing the follow-up. Rustdoc command matrix as before (CI triple
`x86_64-unknown-linux-gnu`; pinned Node for the js build script; doctest
job gets the same Node setup).

Acceptance: worklist complete; matrix builds warning-free with
`RUSTDOCFLAGS="-D warnings"`; the WP8 jsdoc lint (mandatory, scoped)
green.

### WP8: Enforcement (WP8a scaffolding early, WP8b activation last)

WP8a (lands right after WP1):

- `tools/docs-parity` (outside the workspace, own committed lockfile,
  Dependabot cargo entry, nested-lockfile cache key, own README, host
  fmt/clippy/test): the Serde-aware AST extractor (field and
  container/variant attributes including `rename_all`/`tag`/`content`/
  `untagged`, fail-closed on unknown shape-changing attributes, companion
  manifest for custom deserializers, validators, and nonliteral
  defaults), generated-region generator (deterministic ordering, no-write
  check mode, atomic updates), the Cloudflare builder parser (fail-closed
  grammar above), CLI help goldens (both platforms, merged
  platform-annotated union, description override table with owner/
  rationale/expiry/source-text staleness check), snippet-manifest tooling
  (all languages; graded modes incl. `expected_compile_failure` /
  `expected_validation_failure` / `illustrative_fragment`; expiring
  waivers), the domain/credential scanner + typed allowlist, the
  maintained-source manifest checker, and the gate manifest.
- The example harness, redesigned around the secret model: validate the
  template as a deploy-time, key-name-bearing `TrustedServerAppConfig`
  (placeholder strings intact - the template must keep failing deploy
  until customized); serialize the blob envelope; resolve through a fake
  secret store; run runtime validation post-resolution; never substitute
  plaintext into the template itself. Enumerate every `[integrations.*]`
  subtree (grouped by first-segment ID, nested tables in their parent)
  and every `[auction.providers.<id>]` entry - commented or active,
  enabled or disabled - deserializing into the typed structs with
  ignored-key detection, running `Validate::validate`, the profile
  compilers, and the deploy/startup checks via per-ID isolated `Settings`
  fixtures with the integration/provider forced enabled (the runtime path
  skips disabled blocks); valid and invalid compiled probes per profile.
- Inventory equality tests live where visibility allows: module-local
  `#[cfg(test)]` tests inside core assert set equality for the private
  registries (builders, plan registrations, profile registry, mediator,
  JS module sets - replacing the deploy-ID constant's one-directional
  subset check) against the same checked records the tool renders from.

WP8b (lands last):

- Wire everything as blocking CI: rustdoc matrix, native doctests (with
  pinned Node), generated-region clean-diff, snippet manifest, scanner,
  jsdoc lint, gate-manifest check across every surface in its mode,
  repo/orphan/tombstone inventory, disposition-set equality,
  maintained-source manifest equality.
- Workflow edits: CodeQL `rc/*` PR triggers; `.tool-versions` in
  deploy-docs paths; normalized setup-node cache keys (all workflows, to
  lockfiles); Dependabot roots (github-actions, browser and Next.js
  fixture npm, docs-parity cargo); pinned Wrangler; the scheduled
  external link check (pinned checker, job-scoped `issues: write`,
  dedup, auto-close, named owner, fixture-tested);
  `[lints] workspace = true` for openrtb-codegen. Where a governance
  value can be asserted deterministically (a YAML-parsing static test
  over workflow triggers, cache keys, Dependabot roots), it is; the
  remainder is review-time evidence explicitly listed in the PR
  description.
- `CLAUDE.md`/`AGENTS.md`/`TESTING.md`/`docs/guide/testing.md` gate
  regions regenerated from the manifest in the same commit.

Acceptance: every runtime gate has a synthesized negative fixture (dead
link, broken intra-doc link, failing doctest, invalid or unknown-keyed
example block including a disabled integration table and a bad
`profile_config`, planted non-allowlisted domain, unclassified or
expired-waiver snippet fence, missing scoped JSDoc, inventory change
without regenerated region including a macOS-only CLI divergence, missing
README or unlisted orphan, gate mismatch in any manifest surface, removed
ownership marker); the static workflow assertions pass; regenerating all
regions and goldens at final HEAD produces no diff.

## Sequencing

| Order | Package                            | Size | Depends on                |
| ----- | ---------------------------------- | ---- | ------------------------- |
| 0     | WP1 containment subset → `main` PR | XS   | -                         |
| 1     | WP1 hygiene (full, rc PR)          | S    | -                         |
| 2     | WP8a scaffolding                   | L    | -                         |
| 3     | WP2 truth pass                     | M    | WP8a (manifest, scanner)  |
| 4     | WP3 config reference               | L    | WP8a (extractor, harness) |
| 5     | WP4 API reference                  | M    | WP2, WP8a                 |
| 6     | WP5 pages + nav                    | L    | WP2, WP3, WP8a            |
| 7     | WP6 root + READMEs                 | M    | WP2                       |
| 8     | WP7 in-code docs                   | M    | -                         |
| 9     | WP8b gate activation               | M    | WP2-WP7                   |

## Verification

At the final rc-PR HEAD: all GitHub checks green (CodeQL with rc
triggers, format including the docs build, the seven test.yml jobs, the
four integration-test jobs, release builds, JS build/test); the WP8
parity suite and negative fixtures green; regeneration produces no diff;
`cd docs && npm run lint && npm run format && npm run build`; the rustdoc
matrix locally; acceptance greps over the defined sets with output in the
PR description; the four adapter first-success smokes (Axum env bridge,
Fastly local push + secrets, Cloudflare envelope transfer, Spin local
push + variables) executed as documented with commands and cleanup
recorded; the `main` containment PR merged with its positive smoke; the
baseline assertion (merge base equals the recorded SHA) passing.

## Open questions

1. `fastly.toml` `service_id` allowlist owner and review date (blocks
   WP8a scanner activation); the ops migration itself blocks nothing.
2. CNAME: delete (recommended) or custom domain (fully specified branch).
3. `FAQ_POC.md` retirement; gam/kargo tombstones (routes preserved either
   way).
4. `business-use-cases.md`: excluded-with-banner default vs an
   evidence-based rewrite in this pass.
5. CHANGELOG release cut (out of scope; deterministic no-release edit
   defined in Non-goals).
6. Governance ownership (CODEOWNERS/minutes) - blocks the WP6 governance
   edit only.
7. Delivery shape confirmation (blocks starting implementation).
8. CodeQL `push` coverage for `rc/*` (non-blocking).

## Follow-up issues to file (code, not docs)

- `Hooks::stores()` unimplemented on Cloudflare, Spin, and Axum: request-
  time config/KV registries are empty, the declared `TRUSTED_SERVER_KV`
  binding is never opened, Spin's request-signing kid variables are
  unreachable, and `cloudflare.toml` is dead config - wire `stores()` or
  retire the manifests and the stale rustdoc claims (docs fix the rustdoc
  in WP7 either way).
- The Cloudflare Worker does not read the config store `ts config push`
  writes (nested-var startup only), and the CLI has no envelope
  export/output flag - wire the store read or add the export so the
  documented bridge becomes unnecessary.
- `ts serve --adapter axum` does not consume the local config store the
  push writes; the env-var bridge is the documented path.
- Cloudflare registers no `/health`; startup-failure status/health
  behavior differs per adapter (Spin 503 + live health, others 500) -
  decide a uniform contract.
- `imp_ext` reserved-field protection: docs claimed it, code guards only
  `request_ext` - decide whether to guard `imp_ext`.
- `ec.partners[*].ts_pull_token` template placeholder
  (`replace-with-partner-ts-pull-token`) is in no rejection constant
  list.
- `trusted_client_ip.shared_secret` and `tinybird.access_token_secret`
  are `Redacted` but inline in the blob (not in `secret_fields()`) -
  confirm intended or migrate to store references.
- The deploy-ID constant is `#[cfg(test)]` with a one-directional subset
  check (a stale extra entry passes) - superseded by the WP8a
  set-equality test, but the constant itself should be fixed or removed.
- Vendored `edgezero-cli` help text leaks internal spec references into
  `ts config push --help`; fix upstream and bump the pin.
- Tinybird access-log telemetry config present but rejected at runtime;
  auction emission Fastly-only.
- `.env.dev` references an `opid_store` that `fastly.toml` does not
  declare.

## Appendix A: Route inventory (regenerated at `a163367b3`)

Sources: `adapter-fastly/src/{main,app}.rs`, `adapter-axum/src/app.rs`,
`adapter-cloudflare/src/app.rs`, `adapter-spin/src/app.rs`. Symbols cited;
line numbers are hints. WP8 snapshots record response semantics for
guarded/unsupported routes. `trusted_client_ip` is middleware (sanitize
outermost on Axum/Cloudflare/Spin; Fastly sanitizes in `main.rs`; only
Fastly resolves the client IP from it). Spin adds an innermost
`NormalizeMiddleware` (spin-header derivation).

| Route                                  | Methods                         | Fastly                               | Axum          | Cloudflare                      | Spin                               |
| -------------------------------------- | ------------------------------- | ------------------------------------ | ------------- | ------------------------------- | ---------------------------------- |
| `/health`                              | GET                             | pre-router, survives startup failure | real          | **absent** (falls to publisher) | real; also alive in the 503 router |
| `/_ts/debug/ja4`                       | GET                             | pre-router, config-gated             | -             | -                               | -                                  |
| `/.well-known/trusted-server.json`     | GET                             | real                                 | real          | real                            | real                               |
| `/verify-signature`                    | POST                            | real                                 | real          | real                            | real                               |
| `/_ts/admin/keys/{rotate,deactivate}`  | POST                            | real                                 | 501           | 501                             | 501                                |
| `/_ts/admin/ec`, `/_ts/admin/ec/{id}`  | GET                             | real (EC KV)                         | not-supported | not-supported                   | not-supported                      |
| `/_ts/admin/eids`                      | GET                             | real                                 | real          | real                            | real                               |
| `/admin/keys/*` (legacy)               | 7 methods                       | 404 deny                             | 404 deny      | 404 deny                        | 404 deny                           |
| `/_ts/api/v1/batch-sync`               | POST                            | real (Bearer + rate limit)           | -             | -                               | -                                  |
| `/_ts/api/v1/identify`                 | GET, OPTIONS                    | real                                 | -             | -                               | -                                  |
| `/_ts/set-tester`, `/_ts/clear-tester` | GET                             | real (gated)                         | -             | -                               | -                                  |
| `/auction`                             | POST                            | real                                 | real          | real                            | real                               |
| `/_ts/page-bids`, `/__ts/page-bids`    | GET; OPTIONS denied in-handler  | real                                 | real          | real                            | real                               |
| `/first-party/{proxy,click}`           | GET                             | real                                 | real          | real                            | real                               |
| `/first-party/{sign,proxy-rebuild}`    | GET, POST                       | real                                 | real          | real                            | real                               |
| `/static/tsjs=<file>`                  | GET (fallback chain)            | real                                 | real          | real                            | real                               |
| `/integrations/<id>/...`               | varies; families per Appendix C | real                                 | real          | real                            | real                               |
| asset route prefixes                   | GET, HEAD                       | Fastly only                          | -             | -                               | -                                  |
| publisher fallback                     | 7 explicit methods              | real                                 | real          | real                            | real                               |

Startup failure: Spin installs a hardened 503 router (generic body,
`/health` 200, all fallback methods); Cloudflare and Fastly serve 500
from the error router; the Cloudflare and Axum degraded routers answer on
every path. EC partner API, tester cookies, and JA4 remain Fastly-only
(they need platform KV and entry-point wiring). Fastly-only capabilities:
asset routes, image optimizer, request filters (DataDome pre-route),
Tinybird emission. Provider fan-out: Fastly and Axum allow multiple
enabled providers; Cloudflare and Spin reject them at startup (dormant
multi-provider configs are accepted when `auction.enabled = false`).

## Appendix B: Settings inventory (17 roots at `a163367b3`)

`Settings` is `deny_unknown_fields`; `Option` roots: `trusted_client_ip`,
`request_signing`, `creative_opportunities`.

| #   | Root                             | example.toml                    | configuration.md     |
| --- | -------------------------------- | ------------------------------- | -------------------- |
| 1   | `publisher`                      | active                          | yes                  |
| 2   | `tester_cookie`                  | commented                       | yes                  |
| 3   | `trusted_client_ip` (Opt)        | commented                       | yes                  |
| 4   | `ec`                             | active (partners commented)     | yes                  |
| 5   | `integrations`                   | mixed (5 active stubs)          | 5 of 14 subsections  |
| 6   | `handlers`                       | active                          | yes                  |
| 7   | `response_headers`               | commented                       | yes                  |
| 8   | `request_signing` (Opt)          | commented                       | yes                  |
| 9   | `rewrite`                        | commented                       | yes                  |
| 10  | `auction` (+ providers, bidders) | active incl. `pbs-main`         | yes (config-first)   |
| 11  | `consent`                        | commented                       | **missing section**  |
| 12  | `cache`                          | commented rules                 | yes                  |
| 13  | `proxy`                          | header active, leaves commented | yes                  |
| 14  | `creative_opportunities` (Opt)   | active                          | yes                  |
| 15  | `image_optimizer`                | commented                       | nested under Proxy   |
| 16  | `tinybird`                       | commented                       | **Quick Start only** |
| 17  | `debug`                          | commented (+ comment options)   | **missing section**  |

The docs "Key Sections" table lists 10 of 17. Secret model: 11
`secret_fields()` paths, all `KeyInDefault` (3 required:
`publisher.proxy_secret`, `ec.passphrase`, `handlers[*].password`);
resolution flow is verify → `remove_inactive_secret_references` →
`resolve_secret_references` → `from_json_value` →
`validate_settings_for_runtime`; deploy validation excludes secret-leaf
attributes and requires key names conditionally. Redacted-but-inline:
`trusted_client_ip.shared_secret`, `tinybird.access_token_secret`.
Template placeholder constants and their three literal-string test call
sites (the config.rs template tests, the CLI ad_templates substitutions,
the `config init` byte source) are load-bearing. Validator inventory for
companion entries: 2 struct-level schema validators
(`validate_trusted_client_ip`, APS inventory-identity override), 23
field-level custom-validator sites, the imperative
`finalize_deserialized` pipeline (normalize → prepare_runtime → derive
validate → admin coverage → placeholder rejection), the plan-compiler
family (`ProviderId`/`BidderId` grammars, `canonicalize_endpoint`,
notification caps, mediator match, signing gate,
`validate_for_target`), the profile compilers (standard: 16KiB /
depth-8 / 256-key extension limits, `request_ext`-only reserved fields;
prebid-server: override-rule engine; aps: account and inventory
validators), and the integration deploy/startup pairs (DataDome split,
prebid browser-ownership cross-check against the plan, PartnerRegistry
deploy/runtime).

## Appendix C: Integration and provider inventories (at `a163367b3`)

Named sets and counts (set-equality tested in WP8a):

- Deploy-validated IDs: **14** (`validate_enabled_integrations`; the
  `#[cfg(test)]` constant currently checks subset one-directionally).
- Registry `builders()`: **11** - testlight, nextjs, permutive, lockr,
  didomi, sourcepoint, osano, google_tag_manager, datadome, gpt,
  gpt_diagnostics.
- Plan registrations: **2** - `prebid::register_for_plan` and
  `aps::register_for_plan`, prepended by `IntegrationRegistry::with_plan`
  (builders take only `&Settings`; these two need the compiled
  `AuctionPlan`, the single config→runtime boundary shared with the
  orchestrator via one `Arc`); APS registers iff
  `plan.has_profile("aps")`.
- Profile registry: **3** - `standard` (auction-inherited timeout),
  `prebid-server` (1000ms), `aps` (800ms), in `PROFILE_REGISTRATIONS`;
  provider instances are operator-defined `[auction.providers.<id>]`
  (`ProviderId` grammar `^[a-z][a-z0-9-]{0,62}$`; `profile_config` is a
  raw JSON object discriminated by the sibling `profile` field, compiled
  into the profile's typed `deny_unknown_fields` struct) flowing through
  `GenericOpenRtbProvider`.
- Mediator: `adserver_mock` via `register_providers`, matched exactly to
  `auction.mediator`; it never enters the integration registry.
- JS: **12** integration `index.ts` modules (13 dirs; `aps` ships only a
  render helper imported by core/prebid/gpt), **13** emitted bundles
  (+core). `JS_ALWAYS = ["creative"]`. Three loading modes: bundled,
  deferred (prebid only), standalone tag (`gpt_diagnostics`, which is
  `.without_js()` in the registry but served via its own decision path).
- `IntegrationMetadata` omits post-processors, JS modes, and plan info -
  the checked capability record (typed capability + config-predicate
  conditions, fixture matrix evaluating both states) is the rendering
  source; equality tests live module-local where the private registries
  are visible.

Capabilities (P proxy, AR attribute rewriter, SR script rewriter, HI head
injector, PP post-processor, RF request filter): prebid P/AR/HI +
deferred JS; aps HI always, P conditional on `trusted_server` rendering
mode, no JS bundle; testlight P/AR; nextjs SRx2/PP, no JS; permutive
P/AR; lockr P/AR; didomi P/HI; sourcepoint P/AR/HI; osano bare;
google_tag_manager P/AR/SR; datadome P/AR/HI + RF conditional on
`enable_protection`; gpt P/AR/HI; gpt_diagnostics bare + standalone JS;
creative JS-only, always injected.

## Appendix D: CLI and per-adapter configuration handoff

`ts` (the two-platform help union is canonical; macOS adds `dev proxy`):
`audit page|generate|ad-templates generate|verify`, `active-version`,
`auth login|logout|status`, `build`,
`config init|diff|push|validate|gc|ad-templates lint|match|check|explain`,
`deploy`, `healthcheck`, `prebid bundle`, `provision`, `rollback`,
`serve`, `dev proxy [ca ...]`. `--version` is available. Drift-detecting
commands use a stable drift exit code. rc's cli.md already covers the
lifecycle commands; the remaining work is the generated-region conversion
and description gating.

Per-adapter configuration handoff (deployment-guide truth):

- Fastly: config store `trusted_server_config` + secret store
  `ts_secrets` (logical mapping via the `edgezero_runtime_env` config
  store); `ts config push --local` mutates `fastly.toml`; the checked-in
  store ships empty, so a bare `fastly compute serve` serves 500 on
  non-health paths.
- Axum: config via
  `TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG`,
  secrets via `TRUSTED_SERVER_SECRET_{STORE}_{KEY}`.
- Cloudflare: `TRUSTED_SERVER_CONFIG` var = `{"app_config": "<serialized
envelope string>"}` (nested wrapper) + `wrangler secret put <key>`;
  push-written KV is unread by the Worker.
- Spin: blob in KV store label `default` (push requires
  `EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME=default` or it
  lands in the wrong store); secrets via encoded Spin variables
  (`v_<store>_v_<key>` encoding; empty defaults fail closed).

Runtime environment variables and the `TRUSTED_SERVER__` CLI overlay are
documented as separate surfaces (WP2).

## Appendix E: Still-open finding index (verified 2026-08-27)

- `srcExclude` absent; 127 superpowers files in the CI-built site; empty
  `guide/index.md`; nav Guide/Business Value links; CNAME placeholder.
- `fastly.toml:4,10,38` + inconsistent fixture labels;
  `docs/package.json` ISC/not-private; onboarding published.
- `RequestWrapper` (`architecture.md:93-104`,
  `.claude/agents/code-architect.md:16`); Equativ (`ad-serving.md:11`,
  `.claude/agents/issue-creator.md:85`, `FAQ_POC.md`); `.with_asset`
  (`creative-processing.md:808`, `integration-guide.md:84,248`);
  `npm run type-check` (`error-reference.md:614`);
  `settings_data::get_settings` (`configuration.md:2287`); auction README
  rot; `SEQUENCE.md` links; `TESTING.md` runbook.
- `[consent]`/`[debug]` sections missing; `[tinybird]` Quick-Start-only;
  Key Sections 10/17; reserved-field `imp_ext` docs/code mismatch;
  integration subsections 5/14; `adserver_mock` stranded.
- CHANGELOG: 8 breaking entries, inconsistent markers, dead v1.2.0 links.
- Stale rustdoc: the Cloudflare `platform.rs:579-592` stores claim (and
  its Spin sibling); `cloudflare.toml` dead; false workflow comments
  (Spin release build, test-cli default target).
- Env files carry retired `TRUSTED_SERVER__SYNTHETIC__*` keys;
  `opid_store` mismatch.
- Missing pages: cloudflare/spin/axum-dev/edgezero/telemetry/tsjs/
  adserver_mock guides; 7 crate READMEs; integration-guide snippets do
  not compile.
- Enforcement gaps: no rustdoc/doctests in CI, no parity checks, jsdoc
  inert, openrtb-codegen lints, PR template `tracing`, slash-command gate
  drift.
