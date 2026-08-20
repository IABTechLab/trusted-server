# Documentation Refresh (Full Surface)

**Date:** 2026-08-19
**Status:** Draft, pending review
**Scope:** Documentation and doc tooling only. No runtime behavior changes. Baseline audited at `main` commit `2e85a1cdc` (2026-08-18).

## Context

Trusted Server's documentation spans four surfaces: the VitePress site
(`docs/`), root and per-crate markdown, in-code documentation (rustdoc, clap
help, JSDoc), and configuration templates (`trusted-server.example.toml`,
`fastly.toml`, `edgezero.toml`, `.env.example`). A four-track audit of `main`
found systemic drift in every surface. The failures fall into five categories:

1. **Fabricated or dead content presented as real.** The API reference
   documents `GET /first-party/ad` and `POST /third-party/ad`; neither exists
   anywhere in `crates/` (the real client auction endpoint is `POST /auction`).
   `docs/guide/ad-serving.md` documents an Equativ ad server and an
   `[ad_servers.equativ]` config key with zero code presence.
   `docs/guide/architecture.md:97-104` shows a `RequestWrapper` trait that does
   not exist. Sidebar-linked pages exist for `gam` ("Target Release: Q1 2026",
   now past) and `kargo` integrations that have no implementation.
   `crates/trusted-server-core/src/auction/README.md` cites a route table in
   `main.rs` with invented line numbers (routes moved to `app.rs`), a
   `providers/` directory that does not exist, and an APS `mock` config key
   that was removed.
2. **Incomplete references.** Five of fifteen `Settings` sections
   (`tinybird`, `consent`, `tester_cookie`, `image_optimizer`, `rewrite`) are
   absent from both `trusted-server.example.toml` and
   `docs/guide/configuration.md`. The API reference omits `POST /auction`,
   `/_ts/page-bids`, `/health`, `/_ts/debug/ja4`, the EC partner API, and all
   integration endpoints except three. `docs/guide/cli.md` omits
   `ts config diff` and the entire `ts dev` subtree.
   `docs/guide/integrations-overview.md` compares 7 of 14 integration IDs.
   `docs/guide/architecture.md` describes 4 of 10 workspace crates.
3. **Missing coverage.** No pages exist for: Cloudflare/Spin/Axum deployment
   (only Fastly has a setup guide, and it is orphaned from the nav), EdgeZero
   platform layer, auction telemetry/Tinybird (a 17-file `tinybird/` directory
   with no operator path to a working config), the tsjs module system, GPT
   slot handoff, cross-adapter parity testing, `testlight` (the example/test
   integration the integration guide mirrors), and `adserver_mock`. Seven of ten crates have no
   README, including all four adapters and the CLI.
4. **Publishing and policy hygiene.** All 75 internal spec/plan files under
   `docs/superpowers/` are built and published to the public GitHub Pages site
   (no `srcExclude` in `docs/.vitepress/config.mts`), along with internal
   onboarding, an internal epic, and an ops runbook. `docs/public/CNAME`
   contains the literal placeholder `your-custom-domain.com`. `fastly.toml`
   carries a real personal email (`authors`, line 4) and a real Fastly service
   id (line 10) against the repo's own sensitive-data policy, and unlabeled
   base64 key fixtures that read as credentials.
5. **No enforcement.** `cargo doc` never runs in CI; the two existing
   doctests never execute (core is tested only cross-compiled, which skips
   doctests); no `missing_docs` or `rustdoc::*` lints are enabled; the docs
   PR workflow runs lint and Prettier but never `vitepress build`, so dead
   links are only caught after merge when the deploy breaks the live site;
   `eslint-plugin-jsdoc` is installed but has zero rules enabled.

Full finding indexes with `file:line` citations are in Appendix E.

## Decision

Treat documentation as a product surface with a defined source of truth per
artifact, fix the audit findings in eight independently shippable work
packages ordered by risk, and add CI enforcement so the same drift cannot
silently recur. Every claim in the refreshed docs must be verifiable against
code on `main`; anything aspirational must be labeled as such or removed.

The source-of-truth map:

| Artifact           | Truth source                                                                                              | Consumers               |
| ------------------ | --------------------------------------------------------------------------------------------------------- | ----------------------- |
| HTTP API reference | Adapter route tables (`adapter-*/src/app.rs`) + core handlers                                             | Publishers, partners    |
| Config reference   | `Settings` in `crates/trusted-server-core/src/settings.rs` (`deny_unknown_fields` makes parity checkable) | Operators               |
| CLI reference      | clap definitions in `crates/trusted-server-cli/src/run.rs` and command modules                            | Operators               |
| Integration pages  | `builders()` in `core/src/integrations/mod.rs` + registry capabilities                                    | Publishers, integrators |
| Deployment guides  | `edgezero.toml` adapter blocks + per-adapter manifests                                                    | Operators               |
| Architecture       | `Cargo.toml` workspace members + `core/src/platform/`                                                     | Contributors            |
| Test/CI docs       | `.cargo/config.toml` aliases + `.github/workflows/*`                                                      | Contributors            |

## Goals

- Every endpoint, config key, command, flag, crate name, and code path named
  in documentation exists in the code at `main`, with adapter-specific
  availability stated where routes differ (several are Fastly-only).
- Every shipped, operator- or publisher-visible surface has documentation:
  all 14 integration IDs, all 15 config sections, all 4 deployment targets,
  all `ts` commands, the telemetry pipeline, and the tsjs module system.
- The public docs site publishes only intended pages: internal specs, plans,
  epics, onboarding, and runbooks are excluded from the build.
- Sensitive real-world values are removed from source-controlled config per
  the repo policy in `CLAUDE.md`.
- CI gates catch documentation regressions: docs build (dead links) on PRs,
  rustdoc build with broken-intra-doc-link denial, doctests actually running.
- Root markdown (`README`, `CONTRIBUTING`, `TESTING`, `CHANGELOG`) accurately
  describes the current workspace, build system, and test matrix.

## Non-goals

- No changes to runtime behavior, routes, config schema, or code structure.
  Where the audit exposed code issues (e.g. `ts --version` missing, Tinybird
  access logging config present but not wired, internal spec references
  leaking into vendored `edgezero-cli` help text), this spec records them as
  follow-up issues, not in-scope work.
- No new documentation toolchains. VitePress, rustdoc, and clap help remain
  the three delivery mechanisms. No TypeDoc, no docs.rs publishing.
- No rewrite of `docs/roadmap.md` content strategy or
  `docs/business-use-cases.md` marketing copy beyond factual corrections
  (crate names, adapter maturity).
- No release-management policy changes. The CHANGELOG's 10-month untagged
  `[Unreleased]` backlog and the governance doc's unfulfilled commitments are
  flagged for maintainers, with only mechanical repairs in scope.
- Not chasing 100% rustdoc item coverage. In-code doc work targets module
  orientation (`//!`) and the highest-traffic public surfaces, not a
  `missing_docs` blanket.

## Work packages

All eight packages ship in one single implementation PR. Each package is one
commit (or a small commit series) in the order below, so the PR is
reviewable commit-by-commit: WP1 and WP2 are corrective, WP3-WP6 are
completion work, WP7-WP8 are quality and enforcement.

### WP1: Publishing and policy hygiene

Smallest package, highest urgency.

- Add `srcExclude: ['superpowers/**', 'internal/**', 'epics/**', 'README.md']`
  to `docs/.vitepress/config.mts` so internal material stays in the repo but
  out of the published site. Verify with a local `vitepress build` that the
  dist no longer contains those paths.
- Resolve `docs/public/CNAME`: it currently ships the placeholder
  `your-custom-domain.com` into every Pages deploy while `base` is set to
  `/trusted-server` (the two are mutually inconsistent). Default action:
  delete the CNAME and keep the project-path deploy; revisit if a custom
  domain is actually provisioned. Update `docs/README.md:138` and `:172`
  accordingly.
- `fastly.toml`: replace the personal email in `authors` with an empty list
  (matching the workspace `Cargo.toml`); remove or externalize the hardcoded `service_id` (confirm the
  deploy workflow's expectations first; the CHANGELOG already claims this
  removal happened); add `# local test fixture, not a real key` labels to the
  `[local_server]` secret/JWKS entries; add one-line comments to the four KV
  store declarations; remove the orphaned reference to the deleted
  `scripts/test-prebid-eids.sh` (line 38).
- `.github/pull_request_template.md`: fix `tracing` to `log` (line 40); add
  Cloudflare, Spin, and parity gates to the test-plan checkboxes.
- Fill `docs/guide/index.md` (currently 0 bytes, renders a blank page) with a
  short guide landing page linking to Getting Started, Architecture,
  Configuration, and the integration index.
- Align `.claude/commands/{check-ci,verify,test-all,test-crate}.md` with the
  canonical gate list in `CLAUDE.md` (all currently omit Spin and
  `clippy-cloudflare-wasm`; `test-crate.md` uses an untargeted
  `cargo test -p`, the exact pattern `AGENTS.md` warns will fail). Add the
  missing Spin/cloudflare-wasm gates to `AGENTS.md`'s fallback list.

Acceptance: `vitepress build` output contains no `superpowers/`, `internal/`,
or `epics/` pages; no real personal emails or service ids in tracked config;
every command file lists the same gates as `CLAUDE.md`.

### WP2: Remove fabricated and dead content

Truth pass over existing pages. Nothing new is written here beyond minimal
replacement prose; the goal is that nothing documented is false.

- `docs/guide/api-reference.md`: delete `GET /first-party/ad` and
  `POST /third-party/ad` sections (endpoints do not exist). The full
  replacement reference is WP4; in this package, add a stub for
  `POST /auction` so the primary endpoint is not undocumented in the interim.
- `docs/guide/integrations-overview.md:46-48`: remove the same dead routes.
- `docs/guide/ad-serving.md`: remove the Equativ section, the
  `[ad_servers.equativ]` block, the top-level `[prebid]` block (real section
  is `[integrations.prebid]`), and the placeholder `trackImpression` API;
  also remove the `equativ` bidder from the example in
  `docs/guide/integration-guide.md:313`.
  Rewrite the page as a short, accurate description of the real flow:
  creative opportunities matched during HTML processing, server-side auction,
  creative rewriting to the first-party proxy, GPT handoff.
- `docs/guide/architecture.md`: remove the nonexistent `RequestWrapper` trait
  example; replace with the real platform traits from
  `core/src/platform/traits.rs` (`PlatformKvStore`, `PlatformConfigStore`,
  `PlatformHttpClient`, ...).
- Adapter maturity consistency: `docs/index.md:27`,
  `docs/guide/what-is-trusted-server.md:32`, and `docs/roadmap.md:19,34-38`
  all describe Cloudflare/Spin as future work while
  `docs/guide/architecture.md:154-159` calls them production targets. Settle
  on the architecture page's version (all four adapters ship with CI) and fix
  the other three. Fix `docs/roadmap.md:21-22` old crate names (`/common/`,
  `/cloudflare/`).
- Retire `docs/guide/integrations/gam.md` and `kargo.md` (delete pages,
  remove sidebar entries). Neither integration exists; GAM ad serving is
  already covered factually via GPT/creative-opportunities docs. If the team
  wants to keep roadmap visibility, a one-line entry in `roadmap.md` replaces
  each page.
- Retire `FAQ_POC.md`: its headline answer ("NOT ready for use", two-partner
  Fastly+Equativ POC) is false on every axis. Delete it (git history
  preserves it); fold any still-true answers into
  `docs/guide/what-is-trusted-server.md`.
- `crates/trusted-server-core/src/auction/README.md`: point the route table
  at `crates/trusted-server-adapter-fastly/src/app.rs` and drop the invented
  line numbers (name the tables, `NAMED_ROUTES` / `routes_for_state()`,
  instead of line numbers so this cannot rot the same way); remove the
  `providers/your_provider.rs` instructions in favor of the real layout
  (`auction/provider.rs`, provider registration via
  `register_providers` in each integration); delete the APS `mock = true`
  sections (field no longer exists).
- `docs/guide/onboarding.md:51,107`: remove or retarget the two links to the
  nonexistent root `SEQUENCE.md`.
- `docs/epics/revenue-operations-dashboard.md`: correct its telemetry
  sections to reflect the shipped Tinybird pipeline (the epic proposes
  BigQuery/Grafana/Prometheus and predates it). It is excluded from the site
  by WP1 either way.
- `CHANGELOG.md` mechanical repairs: fix the two dead compare links (no
  `v1.2.0` tag exists), the `.rust-analyzer.json` reference (file does not
  exist), the retired `synthetic_id`/`x-synthetic-id` naming (subsystem is
  now EC), section ordering per Keep-a-Changelog, the "fastly.tom" and
  "gogernance" typos, and add the missing entry for #992 (DataDome IP
  exclusions and staging bypass, an operator-visible change).
- `.env.example`: remove the `TRUSTED_SERVER__SYNTHETIC__*` keys and the
  implication that the `TRUSTED_SERVER__` overlay configures the runtime (the
  runtime loads config from the store; the env overlay is test-only). Document
  the variables the runtime actually reads (Appendix D) and reference
  `.env.example` from the getting-started guide, which today only mentions
  `.env.dev`.
- `docs/guide/getting-started.md:141`: `[gdpr]` does not exist; the section
  is `[consent]`.

Acceptance: grepping all tracked markdown and example config (the docs tree
plus root files and crate READMEs) for `first-party/ad`, `third-party/ad`,
`equativ`, `ad_servers`, `RequestWrapper`, `trackImpression`, `SEQUENCE.md`,
`synthetic_id`, `providers/your_provider`, and `mock = true` (APS context)
returns nothing; no sidebar entry points at a nonexistent integration.

### WP3: Configuration reference completion

Bring the two operator-facing config artifacts to parity with `Settings`
(`core/src/settings.rs:1916`, `#[serde(deny_unknown_fields)]`).

- `trusted-server.example.toml`: add commented, documented example blocks for
  the missing sections: `[tinybird]` (all 10 fields, with the note that
  `access_enabled` must remain false), `[consent]` (mode, expiration,
  jurisdiction, conflict resolution, `consent_store`), `[rewrite]`
  (`exclude_domains`, already referenced by a CHANGELOG breaking entry),
  `[tester_cookie]`, `[image_optimizer]` (`profile_sets` with one worked
  profile), `[[proxy.asset_routes]]` (one worked route with `path_pattern`
  and optional S3 SigV4 auth block), `[integrations.osano]`, and the missing
  `[auction]` keys (`mediator`, `creative_store`,
  `allowed_context_keys`) plus `[debug].inject_adm_for_testing` with its
  never-in-production warning.
- `docs/guide/configuration.md`: add the missing `### [consent]`,
  `### [tinybird]`, and `### [debug]` sections; extend the Integration
  Configurations section from 5 to all 14 IDs (add `aps`, `datadome`,
  `didomi`, `sourcepoint`, `lockr`, `gpt`, `gpt_diagnostics`,
  `google_tag_manager`, `adserver_mock`), each with its typed config keys
  from the integration source.
- Add a parity checklist to the implementation PR description mapping each
  of the 15 `Settings` fields to its example-toml block and configuration.md
  heading (the table in Appendix B is the worklist).

Acceptance: every field of `Settings` appears in both
`trusted-server.example.toml` and `docs/guide/configuration.md`; every
integration ID accepted by deploy validation (`core/src/config.rs:29-44`) has
a config subsection.

### WP4: API reference rebuild

Rebuild `docs/guide/api-reference.md` from the route inventory (Appendix A).

- Document every named route: health, discovery/signing endpoints, admin key
  rotation (and the deliberately 404-denied legacy `/admin/keys/*` aliases),
  EC partner API (`/_ts/api/v1/batch-sync`, `/_ts/api/v1/identify`), tester
  cookie endpoints, `POST /auction`, `GET /_ts/page-bids` plus the legacy
  `/__ts/page-bids` alias, the four
  `/first-party/*` proxy endpoints, `/_ts/debug/ja4`, and the tsjs bundle
  endpoint (`/static/tsjs=...`, unified vs deferred vs standalone module
  forms with ETag behavior).
- Add an adapter-availability matrix: several routes are Fastly-only
  (EC partner API, tester cookies, JA4 debug, real key rotation, Tinybird
  telemetry), `/health` is absent on Cloudflare, and Axum returns
  `admin_key_management_not_supported`. This distinction exists nowhere in
  the docs today.
- Document the fallback dispatch order (tsjs, integration proxy routes,
  asset routes, publisher origin proxy) so route-shadowing questions are
  answerable from docs.
- Add an Integration Endpoints section generated from each integration's
  `IntegrationProxy::routes()` registration (the integrations are enumerated
  in Appendix C) instead of today's three-entry list.
- State auth expectations per route group: Basic auth handlers covering
  `Settings::ADMIN_ENDPOINTS`, Bearer auth on the partner API, `tstoken`
  signing on first-party proxy URLs.

Acceptance: the route list in the reference matches the union of the four
adapter route tables, with per-adapter availability flagged; every documented
route names its handler file.

### WP5: New coverage pages and navigation repair

- New deployment guides parallel to `docs/guide/fastly.md`:
  `docs/guide/cloudflare.md` (wrangler config, `TRUSTED_SERVER_KV` binding,
  `TRUSTED_SERVER_CONFIG` var with blob envelope, missing `/health`),
  `docs/guide/spin.md` (component variables encoding, `spin-full-url`
  reconstruction, KV store), and `docs/guide/axum-dev.md` (env-var-backed
  stores, `PORT`, unsupported admin ops). Group all four under a new
  "Deployment" sidebar section and stop orphaning `fastly.md`.
- New `docs/guide/edgezero.md`: the platform layer the app now sits on. The
  `edgezero.toml` manifest (app, logical stores, adapter blocks), the config
  flow (`trusted-server.toml` validated, pushed as a blob envelope via
  `ts config push`, resolved at runtime through `settings_data.rs` including
  Fastly chunked storage), and the `ts` lifecycle commands
  (auth/build/serve/deploy/provision). Fold the still-relevant parts of
  `docs/internal/EDGEZERO_MIGRATION.md` in; the internal runbook itself stays
  excluded from the site.
- New `docs/guide/telemetry.md`: auction telemetry from
  `[tinybird]` config through `auction_sink_from_settings` to the
  `tinybird/` datasources, pipes, and rollups; the operator setup path
  (Tinybird tokens in `ts_secrets`); explicit note that access-log telemetry
  is not yet wired and `access_enabled` must remain false. New
  `tinybird/README.md` covering the `tb` workflow and file layout.
- New `docs/guide/tsjs.md`: the module system (core + immediate vs deferred
  integration modules, `JS_ALWAYS` creative module), the build pipeline
  (`build-all.mjs`, `build.rs` embedding, runtime concatenation and
  hashing), the bundle endpoint forms, the SPA page-bids flow, and the
  public `window.tsjs` surface from `crates/trusted-server-js/lib/src/core/types.ts`.
- Document `testlight` in its real context: it is the example/test
  integration, so it belongs in the developer-facing
  `docs/guide/integration-guide.md` (which already mirrors it) as a named
  reference section, not as a partner integration page. Add
  `docs/guide/integrations/adserver_mock.md` for the mock auction provider
  used in local development, currently unmentioned in all docs.
- Extend `docs/guide/integrations/gpt.md` with the slot handoff mechanism
  (edge-injected `gpt_bootstrap.js`, the full shim takeover, targeting, APS
  renderer bridge, SPA hook); "handoff" currently appears nowhere in docs.
- `docs/guide/integrations-overview.md`: extend the comparison and
  performance tables from 7 to all 14 IDs using the registry capability
  matrix (Appendix C).
- Testing docs: rewrite root `TESTING.md` as the test-matrix index (the
  aliases from `.cargo/config.toml`, the seven `test.yml` jobs plus the four
  integration-test workflow jobs, the parity
  suite, `scripts/test-cli.sh`, integration/browser scripts, vitest), and
  move its current content, an auction curl runbook, into
  `docs/guide/auction-testing.md` cross-linked from the auction README.
  Update `docs/guide/testing.md` to cover cloudflare/spin/parity/CLI/browser
  suites and replace the fictional two-job CI YAML with the real seven-job
  layout.
- `docs/guide/cli.md`: full command reference from the clap tree (Appendix
  D), adding `ts config diff` and the `ts dev` subtree with its macOS-only
  gating, and linking to `ts-dev-proxy.md`.
- Navigation: add sidebar entries for the three orphaned real integrations
  (`gpt`, `google_tag_manager`, `sourcepoint`) and the new pages; decide
  placement for `onboarding.md` (internal; excluded by WP1 unless moved).
- `docs/guide/architecture.md`: describe all 10 workspace crates and the
  platform trait boundary; add the missing Cloudflare adapter section.

Acceptance: every integration ID is documented and nav-reachable (testlight
via its reference section in the integration guide); every deployment target
has a guide; no real page is orphaned; `vitepress build` passes (dead links
fail the build).

### WP6: Root markdown and crate READMEs

- `README.md`: current quick start including the `ts` CLI path
  (`ts config init` / `ts serve --adapter ...`) alongside `fastly compute
serve`; link the four deployment guides; refresh the doc-site link table.
- `CONTRIBUTING.md` (untouched since 2026-01): reference the per-target
  alias system and full CI gate list, point to `CLAUDE.md`/`AGENTS.md` for
  agent workflows, fix the "could be dev/develop/master" boilerplate.
- New crate READMEs (short, orientation-level: what it is, how it builds,
  where its docs live) for the seven crates lacking one:
  `trusted-server-adapter-fastly`, `-axum`, `-cloudflare`, `-spin`,
  `trusted-server-cli`, `trusted-server-js`, `trusted-server-openrtb-codegen`.
  Rewrite `crates/trusted-server-core/README.md` as an actual crate overview
  (currently covers 2 of ~40 modules), linking to the deep-dive docs.
- New `scripts/README.md` (one line per script).
- `ProjectGovernance.md`: the two claims contradicted by repo state
  (meeting minutes "maintained within the repository" - none exist;
  "continuous releases" - none tagged since v1.1.0) become accurate
  statements of intent, unless open question 6 resolves them differently;
  link it from `CONTRIBUTING.md` so the governance model is
  visible at the contribution point. Naming maintainers/CODEOWNERS is a
  maintainer decision, flagged as an open question.
- Add `readme = "README.md"` to each crate's `Cargo.toml` once the READMEs
  exist.

Acceptance: `find crates -maxdepth 2 -name README.md` returns one per crate;
README quick start commands all run against `main`.

### WP7: In-code documentation

Targeted, not exhaustive. Priorities in order:

1. `core/src/lib.rs` module index: currently lists 12 of 40+ public modules
   and links a `test_support` module; make it complete and grouped
   (identity, consent, auction, HTML pipeline, proxy, platform, config).
2. `core/src/platform/` (2/8 files documented): module docs for `traits.rs`,
   `types.rs`, `kv.rs`, `http.rs`, `error.rs`. This is the cross-adapter
   contract and the highest-value rustdoc gap in the repo.
3. Crate-level `//!` headers for the crates missing them:
   `adapter-fastly` (`main.rs`), `adapter-cloudflare`, `trusted-server-js`,
   and `trusted-server-cli` (whose `lib.rs` already contains the right prose
   as `//` comments; convert to `//!`).
4. Module docs for the undocumented operator/security-relevant core files:
   `settings.rs`, `http_util.rs`, `proxy.rs`, `auth.rs`, `tsjs.rs`,
   `html_processor.rs` (expand the 3-line header for a 1000-line streaming
   rewriter), `integrations/registry.rs`, `integrations/prebid.rs`, and the
   `nextjs/` and `datadome/` subtrees.
5. `core/src/constants.rs`: document the 35 undocumented public constants
   (cookie and header names are de facto public API).
6. CLI module docs for `commands/audit/*`, `commands/config/*`, `run.rs`.
7. TypeScript: file-header JSDoc for the zero-doc multi-export files
   (`core/render.ts`, `shared/globals.ts`, `core/registry.ts`,
   `integrations/creative/*`), and complete `core/types.ts` (17/35 exports
   documented), which is the public tsjs type surface. Add a header block to
   `build-prebid-external.mjs` (401 lines, no header).

Style follows `CLAUDE.md` documentation standards. `# Examples` sections are
added only where an example compiles as a doctest and earns its keep
(`redacted.rs` is the model); this spec does not attempt examples on all ~589
public functions.

Acceptance: `cargo doc --no-deps` builds warning-free for core (native) and
each adapter (per target); every workspace crate and every `pub mod` in core
has a `//!` header.

### WP8: Enforcement

Prevent recurrence. All additions gate on existing tooling; no new services.

- Docs site: add `npm run build` to the `format-docs` job in
  `.github/workflows/format.yml` so dead links fail PRs instead of the
  post-merge deploy. Align the two workflows' npm cache keys (one keys on
  `package.json`, the other on `package-lock.json`).
- Rustdoc: add a CI step running `cargo doc --no-deps` for
  `trusted-server-core` plus the adapters on their matching targets with
  `RUSTDOCFLAGS="-D warnings"` (this denies `rustdoc::broken_intra_doc_links`
  by default). Do not enable `missing_docs`; the existing
  `missing_errors_doc`/`missing_panics_doc`/`doc_markdown` clippy trio plus
  `-D warnings` stays the item-level gate.
- Doctests: add a native-host `cargo test --doc -p trusted-server-core` step
  (doctests are silently skipped today because core is only tested
  cross-compiled).
- Add `[lints] workspace = true` to `trusted-server-openrtb-codegen`, the
  one crate not inheriting the doc lints.
- Dependabot: add the `github-actions` ecosystem and the Playwright
  `browser/package.json` npm root (both currently unmanaged).
- `CLAUDE.md`: update the CI Gates section to the real gate list (it omits
  ESLint, the CLI/codegen clippy jobs, the bench compile check, the release
  WASM builds, and the entire integration-tests workflow) so agents and the
  slash commands stay aligned with reality. Keep `MEMORY.md`-tracked crate
  paths out of scope; this spec only touches repo files.
- Optional, decide at review: enable a minimal `jsdoc/*` ESLint rule set
  (e.g. `jsdoc/check-alignment`, `jsdoc/check-types`) now that the plugin is
  installed; skipped by default to keep WP8 low-noise.

Acceptance: a PR introducing a dead docs link, a broken intra-doc link, or a
failing doctest fails CI.

## Sequencing and estimate

| Order | Package                  | Size | Depends on                      |
| ----- | ------------------------ | ---- | ------------------------------- |
| 1     | WP1 hygiene              | S    | -                               |
| 2     | WP2 truth pass           | M    | -                               |
| 3     | WP3 config reference     | M    | -                               |
| 4     | WP4 API reference        | M    | WP2                             |
| 5     | WP5 new pages + nav      | L    | WP2 (nav), WP3 (links)          |
| 6     | WP6 root + crate READMEs | M    | -                               |
| 7     | WP7 in-code docs         | M    | -                               |
| 8     | WP8 enforcement          | S    | WP7 (doc build must pass first) |

Commits land in this order within the single implementation PR; WP8 comes
last so the new CI gates turn green on the same PR.

## Verification

For the implementation PR:
`cd docs && npm run lint && npm run format && npm run build`;
`cargo fmt --all -- --check`; the target-matched clippy/test aliases for any
crate whose source files changed (WP7); `cargo doc --no-deps` locally for
rustdoc changes. The acceptance greps listed in WP2-WP4 are run and their
output included in the PR description. For WP1, a local `vitepress build`
listing of `dist/` proves the exclusion set.

## Open questions

1. `fastly.toml` `service_id`: removal is policy-correct and the CHANGELOG
   claims it already happened, but the deploy path may rely on it. Confirm
   how `fastly compute publish` is invoked in practice before removing.
2. `docs/public/CNAME`: delete (recommended, matches the `/trusted-server`
   base path) or configure a real custom domain?
3. `FAQ_POC.md` and the `gam.md`/`kargo.md` pages: this spec recommends
   deletion; confirm no external links depend on them.
4. `docs/guide/onboarding.md`: exclude from the public site (WP1 default) or
   keep it published?
5. CHANGELOG: should a release be cut to drain the six breaking entries in
   `[Unreleased]`, or should the mechanical repairs land alone? (Mechanical
   repairs are in WP2 either way.)
6. Governance: who owns naming maintainers/CODEOWNERS and the meeting-minutes
   commitment? Out of scope here but flagged.

## Follow-up issues to file (code, not docs)

- `ts --version` does not exist (no `#[command(version)]`).
- Vendored `edgezero-cli` help text leaks internal spec references
  ("5.4", "spec 3.3 Model A") into `ts config push --help`; fix upstream at
  the `edgezero` repo and bump the pinned tag.
- Tinybird access-log telemetry: config exists but is rejected at runtime;
  either wire it or remove the config surface.
- `docs/guide/proxy-signing.md` (oldest page, 2026-01-30) likely needs a
  content review against `core/src/proxy.rs` signing; not audited deeply.

## Appendix A: HTTP route inventory (truth source for WP4)

No single shared router exists; each adapter registers named routes plus a
publisher fallback. Fastly is the superset. Tables: `adapter-fastly/src/app.rs`
(`NAMED_ROUTES`, `routes_for_state()`), `adapter-axum/src/app.rs`
(`named_routes()`), `adapter-cloudflare/src/app.rs` (`build_router()`),
`adapter-spin/src/app.rs` (`named_fallback_paths()`).

| Route                                                                                         | Methods                      | Availability                                       | Handler                                                                         |
| --------------------------------------------------------------------------------------------- | ---------------------------- | -------------------------------------------------- | ------------------------------------------------------------------------------- |
| `/health`                                                                                     | GET                          | all except Cloudflare                              | adapter entry points                                                            |
| `/_ts/debug/ja4`                                                                              | GET                          | Fastly only, gated by `debug.ja4_endpoint_enabled` | `adapter-fastly/src/main.rs`                                                    |
| `/.well-known/trusted-server.json`                                                            | GET                          | all                                                | `core/src/request_signing/endpoints.rs`                                         |
| `/verify-signature`                                                                           | POST                         | all                                                | `core/src/request_signing/endpoints.rs`                                         |
| `/_ts/admin/keys/rotate`, `/_ts/admin/keys/deactivate`                                        | POST                         | Fastly real; Axum/Cloudflare return not-supported  | `core/src/request_signing/endpoints.rs`, `adapter-fastly/src/management_api.rs` |
| `/admin/keys/*`                                                                               | all                          | all: deliberately 404-denied legacy aliases        | adapter apps                                                                    |
| `/_ts/api/v1/batch-sync`                                                                      | POST                         | Fastly only; Bearer auth + rate limit              | `core/src/ec/batch_sync.rs`                                                     |
| `/_ts/api/v1/identify`                                                                        | GET, OPTIONS                 | Fastly only                                        | `core/src/ec/identify.rs`                                                       |
| `/_ts/set-tester`, `/_ts/clear-tester`                                                        | GET                          | Fastly only, gated by `tester_cookie.enabled`      | `core/src/tester_cookie.rs`                                                     |
| `/auction`                                                                                    | POST                         | all                                                | `core/src/auction/endpoints.rs`                                                 |
| `/_ts/page-bids`                                                                              | GET                          | all; gated by `X-TSJS-Page-Bids` header            | `core/src/publisher.rs`                                                         |
| `/__ts/page-bids`                                                                             | GET                          | legacy alias of `/_ts/page-bids`                   | `core/src/publisher.rs`                                                         |
| `/first-party/proxy`, `/first-party/click`, `/first-party/sign`, `/first-party/proxy-rebuild` | GET (sign/rebuild also POST) | all                                                | `core/src/proxy.rs`                                                             |
| `/static/tsjs=<file>`                                                                         | GET                          | all (fallback chain)                               | `core/src/publisher.rs` `handle_tsjs_dynamic`                                   |
| `/integrations/<id>/...`                                                                      | varies                       | per enabled integration (Appendix C)               | integration proxies                                                             |
| asset route prefixes                                                                          | GET, HEAD                    | operator-configured `[[proxy.asset_routes]]`       | `core/src/proxy.rs` `handle_asset_proxy_request`                                |
| everything else                                                                               | all                          | publisher origin proxy + HTML rewriting            | `core/src/publisher.rs` `handle_publisher_request`                              |

Fallback dispatch order: GPT-diagnostics request prep, EC state build and
integration request filters (DataDome may short-circuit), tsjs, integration
proxy routes, asset routes, publisher proxy.

## Appendix B: Settings sections (truth source for WP3)

From `core/src/settings.rs` (`Settings`, line ~1916). Sections marked missing
have no block in `trusted-server.example.toml` today.

| Section                    | Struct                        | Example toml today                                                       |
| -------------------------- | ----------------------------- | ------------------------------------------------------------------------ |
| `[publisher]`              | `Publisher`                   | present                                                                  |
| `[tester_cookie]`          | `TesterCookieConfig`          | missing                                                                  |
| `[ec]`                     | `Ec` + `EcPartner`            | present                                                                  |
| `[integrations.*]`         | per-integration typed configs | partial (osano missing; 9 IDs missing from configuration.md)             |
| `[[handlers]]`             | `Handler`                     | present                                                                  |
| `response_headers`         | map                           | present (commented)                                                      |
| `[request_signing]`        | `RequestSigning`              | present                                                                  |
| `[rewrite]`                | `Rewrite`                     | missing                                                                  |
| `[auction]`                | `AuctionConfig`               | present but missing `mediator`, `creative_store`, `allowed_context_keys` |
| `[consent]`                | `ConsentConfig`               | missing                                                                  |
| `[proxy]`                  | `Proxy`                       | partial; `asset_routes` missing                                          |
| `[creative_opportunities]` | `CreativeOpportunitiesConfig` | present                                                                  |
| `[image_optimizer]`        | `ImageOptimizerSettings`      | missing                                                                  |
| `[tinybird]`               | `TinybirdSettings`            | missing                                                                  |
| `[debug]`                  | `DebugConfig`                 | partial (`inject_adm_for_testing` missing)                               |

## Appendix C: Integration registry (truth source for WP5 overview table)

From `core/src/integrations/mod.rs` `builders()` and per-integration
registrations. Capabilities: P proxy, AR attribute rewriter, SR script
rewriter, HI head injector, PP html post-processor, RF request filter,
DJS deferred JS, AP auction provider.

| ID                   | Capabilities                       | JS module          | Docs page today                 |
| -------------------- | ---------------------------------- | ------------------ | ------------------------------- |
| `prebid`             | P, AR, HI, DJS, AP                 | yes                | in sidebar                      |
| `aps`                | P (renderer), AP, no JS bundle     | render helper only | in sidebar                      |
| `datadome`           | P, AR, HI, RF (when protection on) | yes                | in sidebar                      |
| `gpt`                | P, AR, HI                          | yes                | orphaned                        |
| `gpt_diagnostics`    | standalone JS on demand            | yes                | in sidebar                      |
| `google_tag_manager` | P, AR, SR                          | yes                | orphaned                        |
| `didomi`             | P, HI                              | yes                | in sidebar                      |
| `sourcepoint`        | P, AR, HI                          | yes                | orphaned                        |
| `osano`              | bare registration                  | yes                | in sidebar                      |
| `permutive`          | P, AR                              | yes                | in sidebar (thin)               |
| `lockr`              | P, AR                              | yes                | in sidebar                      |
| `nextjs`             | SR x2, PP, no JS                   | no                 | in sidebar                      |
| `testlight`          | P, AR                              | yes                | none                            |
| `adserver_mock`      | AP only (no registration)          | no                 | none                            |
| `creative` (JS-only) | always injected (`JS_ALWAYS`)      | yes                | covered via creative-processing |

## Appendix D: CLI tree and environment variables

`ts` commands (from `crates/trusted-server-cli/src/run.rs`): `audit`,
`auth login|logout|status`, `build`, `config init|diff|push|validate`,
`deploy`, `prebid bundle`, `provision`, `serve`,
`dev proxy [ca path|install|uninstall|regenerate]` (macOS only; `ts dev`
lists no subcommands on other hosts). All commands and flags carry help text;
`docs/guide/cli.md` must add `config diff` and the `dev` subtree.

Runtime environment variables to document (WP2 `.env.example`):
`FASTLY_SERVICE_VERSION`, `FASTLY_IS_STAGING`, `FASTLY_HOSTNAME`,
`FASTLY_REGION`, `EDGEZERO_LOG_LEVEL`, `PORT` (Axum),
`TRUSTED_SERVER_CONFIG_{STORE}_{KEY}` / `TRUSTED_SERVER_SECRET_{STORE}_{KEY}`
(Axum stores), `TRUSTED_SERVER_CONFIG` (Cloudflare var), `EDGEZERO_*` store
overrides, and the build-time `TSJS_SKIP_BUILD`/`TSJS_TEST`.

## Appendix E: Staleness finding index

Compact index of audit findings driving WP1/WP2; each was verified against
`main` at `2e85a1cdc`.

- Dead endpoints documented: `docs/guide/api-reference.md:85` (`/first-party/ad`),
  `:190` (`/third-party/ad`); `docs/guide/integrations-overview.md:46-48`.
- Fabricated content: `docs/guide/ad-serving.md:11-18,43,48,77-83` (Equativ,
  `[ad_servers]`, `trackImpression`); `docs/guide/architecture.md:97-104`
  (`RequestWrapper`); `docs/guide/integration-guide.md:313` (equativ bidder).
- Wrong config names: `docs/guide/getting-started.md:141` (`[gdpr]`).
- Old crate layout: `docs/roadmap.md:21-22` (the only surviving instance).
- Adapter maturity contradictions: `docs/index.md:27`,
  `docs/guide/what-is-trusted-server.md:32`, `docs/roadmap.md:19,34-38` vs
  `docs/guide/architecture.md:154-159`.
- Aspirational sidebar pages: `docs/guide/integrations/gam.md` (no such
  integration, "Q1 2026" passed), `kargo.md`.
- Auction README: route table file/line rot, nonexistent `providers/` dir,
  removed APS `mock` key (`crates/trusted-server-core/src/auction/README.md:269-285,466-473,487-489,543-549,577`).
- Dead links: `docs/guide/onboarding.md:51,107` (`SEQUENCE.md`);
  `CHANGELOG.md:51-52` (no `v1.2.0` tag).
- CHANGELOG: retired `synthetic_id` naming (`:24`), nonexistent
  `.rust-analyzer.json` (`:51`), missing #992 entry, section order, typos.
- Integration-tests README: wrong CI job name (`:231`), missing environment
  files (`:165-177`), missing browser spec (`:141-145`).
- fastly.toml: personal email (`:4`), service id (`:10`), orphaned script
  reference (`:38`), unlabeled key fixtures (`:48-74`).
- Publishing: 75 `docs/superpowers/**` files built into the public site (no
  `srcExclude`); `docs/public/CNAME` placeholder; empty `docs/guide/index.md`.
- Slash-command drift: `.claude/commands/{check-ci,verify,test-all}.md` omit
  Spin/cloudflare-wasm/parity gates; `test-crate.md` untargeted `cargo test`.
- Tooling: no `cargo doc` in CI; doctests never run (cross-compile only);
  `format-docs` never runs `vitepress build`; `eslint-plugin-jsdoc` inert;
  `openrtb-codegen` missing `[lints] workspace = true`; PR template says
  `tracing`.
