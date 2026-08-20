# Documentation Refresh (Full Surface)

**Date:** 2026-08-19
**Revised:** 2026-08-20 (addresses pre-implementation review)
**Status:** Draft, pending review
**Scope:** Documentation and doc tooling. No runtime behavior changes. Baseline audited at `main` commit `2e85a1cdc` (2026-08-18).

## Context

Trusted Server's documentation spans four surfaces: the VitePress site
(`docs/`), root and per-crate markdown, in-code documentation (rustdoc, clap
help, JSDoc), and configuration templates (`trusted-server.example.toml`,
`fastly.toml`, `edgezero.toml`, `.env.example`, `.env.dev`). A four-track
audit of `main`, hardened by a pre-implementation review, found systemic
drift in every surface. The failures fall into six categories:

1. **Fabricated or dead content presented as real.** The API reference
   documents `GET /first-party/ad` and `POST /third-party/ad`; neither exists
   anywhere in `crates/` (the real client auction endpoint is `POST /auction`).
   The same dead endpoints recur in `docs/guide/error-reference.md:658` and
   `docs/guide/integrations/prebid.md:515-531`.
   `docs/guide/ad-serving.md` documents an Equativ ad server and an
   `[ad_servers.equativ]` config key with zero code presence.
   `docs/guide/architecture.md:97-104` shows a `RequestWrapper` trait that does
   not exist. Sidebar-linked pages exist for `gam` ("Target Release: Q1 2026",
   now past) and `kargo` integrations that have no implementation.
   `crates/trusted-server-core/src/auction/README.md` cites a route table in
   `main.rs` with invented line numbers (routes moved to `app.rs`), a
   `providers/` directory that does not exist, and an APS `mock` config key
   that was removed. Operator instructions reference a nonexistent
   `npm run type-check` (`error-reference.md:597`) and a nonexistent
   `--validate-config` flag (`error-reference.md:663`), and
   `docs/guide/key-rotation.md:301-310` shows an obsolete
   `KeyRotationManager::new(...)?` signature (the real constructor returns
   `Self`, not a `Result`).
2. **Incomplete references.** `docs/guide/configuration.md` has no
   `[consent]`, `[tinybird]`, or `[debug]` sections, and its Integration
   Configurations section covers 5 of 14 IDs. `trusted-server.example.toml`
   has no `[tinybird]`, `[consent]`, `[rewrite]`, `[tester_cookie]`, or
   `[image_optimizer]` blocks. The API reference omits `POST /auction`,
   `/_ts/page-bids`, `/health`, `/_ts/debug/ja4`, the EC partner API, and all
   integration endpoints except three. `docs/guide/cli.md` omits
   `ts config diff` and the entire `ts dev` subtree.
   `docs/guide/integrations-overview.md` compares 7 of 14 integration IDs.
   `docs/guide/architecture.md` describes 4 of 10 workspace crates. The
   integration guide's code snippets do not compile against the current API:
   `IntegrationProxy::handle` is shown without its `RuntimeServices` argument
   (`integration-guide.md:96` vs `registry.rs:282-288`), `proxy_request` is
   shown without its `services` argument (`integration-guide.md:132` vs
   `proxy.rs:737-742`), and a platform-neutral core example imports
   `fastly::http` (`integration-guide.md:134`).
3. **Missing coverage.** No pages exist for: Cloudflare or Spin deployment
   (only Fastly has a setup guide, and it is orphaned from the nav), the
   EdgeZero platform layer, auction telemetry/Tinybird (a 17-file `tinybird/`
   directory with no operator path to a working config), the tsjs module
   system, GPT slot handoff, script guards, cross-adapter parity testing,
   `testlight` (the example/test integration the integration guide mirrors),
   and `adserver_mock`. Seven of ten crates have no README, including all
   adapters and the CLI.
4. **A misleading adapter support model.** The docs describe Cloudflare and
   Spin inconsistently (in-development on the homepage and roadmap,
   production on the architecture page), count Axum as a deployment target
   when it is a local-development adapter with no deploy command, and say
   nothing about the Spin adapter's actual runtime state: it builds its
   settings from the checked-in `trusted-server.example.toml`
   (`adapter-spin/src/app.rs:52`), and a startup failure installs a router
   that returns 503 for all traffic while `/health` still returns 200
   (`adapter-spin/src/app.rs:404`). No smoke test proves non-health traffic
   works under `spin up`. CI compiles the Spin artifact; compilation is not
   evidence of production maturity.
5. **Publishing and policy hygiene.** All 75 internal spec/plan files under
   `docs/superpowers/` are built and published to the public GitHub Pages site
   (no `srcExclude` in `docs/.vitepress/config.mts`), along with
   `docs/guide/onboarding.md` (internal contacts, meetings, access guidance),
   an internal epic, and an ops runbook. `docs/public/CNAME` contains the
   literal placeholder `your-custom-domain.com`. `fastly.toml` carries a real
   personal email (`authors`, line 4) and a real Fastly service id (line 10)
   against the repo's own sensitive-data policy, and unlabeled base64 key
   fixtures that read as credentials. `docs/package.json` is not `private`
   and declares an ISC license in an Apache-2.0 repository.
6. **No enforcement.** `cargo doc` never runs in CI; the two existing
   doctests never execute (core is tested only cross-compiled, which skips
   doctests); no `missing_docs` or `rustdoc::*` lints are enabled; the docs
   PR workflow runs lint and Prettier but never `vitepress build`, so dead
   links are only caught after merge when the deploy breaks the live site;
   `eslint-plugin-jsdoc` is installed but has zero rules enabled. Nothing
   checks that the docs' hand-maintained copies of routes, config fields,
   CLI commands, integration IDs, crate lists, or CI gates match the code,
   which is exactly how the drift above accumulated.

Full finding indexes with `file:line` citations are in Appendix E.

## Decision

Treat documentation as a product surface with a defined source of truth per
artifact, fix the audit findings in eight work packages ordered by risk, and
add enforcement, including executable parity checks, so the same drift is
caught by CI instead of by the next manual audit. Every claim in the
refreshed docs must be verifiable against code on `main`; anything
aspirational must be labeled as such or removed; adapter support claims must
come from an honest, owned support matrix rather than marketing copy.

The source-of-truth map:

| Artifact                   | Truth source                                                                                                                         | Consumers               |
| -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ | ----------------------- |
| HTTP API reference         | Adapter route tables and entry points (`adapter-*/src/app.rs`, `adapter-*/src/main.rs`, `adapter-*/src/platform.rs`) + core handlers | Publishers, partners    |
| Config reference           | `Settings` in `crates/trusted-server-core/src/settings.rs` (`deny_unknown_fields` makes parity checkable)                            | Operators               |
| CLI reference              | clap definitions in `crates/trusted-server-cli/src/run.rs` and command modules                                                       | Operators               |
| Integration pages          | `builders()` in `core/src/integrations/mod.rs` + registry capabilities                                                               | Publishers, integrators |
| Integration guide snippets | A compiling sample integration (`testlight` or a doc-tested fixture), never hand-written pseudo-code                                 | Integrators             |
| Deployment guides          | `edgezero.toml` adapter blocks + per-adapter manifests + adapter support matrix                                                      | Operators               |
| Architecture               | `Cargo.toml` workspace members + `core/src/platform/`                                                                                | Contributors            |
| Test/CI docs               | `.cargo/config.toml` aliases + `.github/workflows/*`                                                                                 | Contributors            |

## Source sets

Truth-pass acceptance criteria and parity checks operate on defined source
sets, not "all tracked files" (the refresh spec itself, and archived specs
under `docs/superpowers/`, legitimately contain every retired term):

- **Active public set:** everything VitePress builds, i.e. `docs/**`
  excluding the WP1 `srcExclude` list. This is what site visitors see.
- **Active repo set:** root markdown (`README.md`, `CONTRIBUTING.md`,
  `TESTING.md`, `CHANGELOG.md`, `ProjectGovernance.md`, `AGENTS.md`,
  `CLAUDE.md`), crate READMEs, config templates
  (`trusted-server.example.toml`, `fastly.toml`, `edgezero.toml`,
  `.env.example`, `.env.dev`), and `.claude/commands/*.md`.
- **Historical set:** `docs/superpowers/**` (specs, plans, implementation
  notes, archive) and shipped `CHANGELOG.md` release entries. Exempt from
  retired-term greps; a changelog entry describing a rename may name the
  old identifier.

## Goals

- Every endpoint, config key, command, flag, crate name, and code path named
  in active-set documentation exists in the code at `main`, with
  adapter-specific availability stated where behavior differs.
- Every shipped, operator- or publisher-visible surface has documentation:
  all 14 integration IDs, all 15 config sections, the deployment adapters
  (with honest maturity labels), all `ts` commands, the telemetry pipeline,
  and the tsjs module system.
- The adapter support model is truthful: three deployment adapters (Fastly
  production; Cloudflare; Spin, currently experimental) plus the Axum
  local-development adapter, backed by a published support matrix.
- The public docs site publishes only intended pages: internal specs, plans,
  epics, onboarding, and runbooks are excluded from the build, and
  internal-only details are scrubbed from anything that stays in the public
  repository regardless of whether VitePress builds it.
- Sensitive real-world values are removed from source-controlled config per
  the repo policy in `CLAUDE.md`.
- CI gates catch documentation regressions: docs build (dead links) on PRs,
  rustdoc build with broken-intra-doc-link denial, doctests actually
  running, and executable parity checks for the hand-maintained inventories.
- Root markdown (`README`, `CONTRIBUTING`, `TESTING`, `CHANGELOG`) accurately
  describes the current workspace, build system, and test matrix.

## Non-goals

- No changes to runtime behavior, routes, config schema, or code structure,
  with one boundary clarification: parity checks added by WP8 may add tests
  and scripts, but not alter runtime code. Code defects the audit exposed
  (Spin's hardcoded example-config startup, `ts --version` missing, Tinybird
  access logging config present but not wired, internal spec references
  leaking into vendored `edgezero-cli` help text) are tracked as follow-up
  issues; the Spin one blocks publishing a Spin deployment guide (WP5).
- No new documentation toolchains. VitePress, rustdoc, and clap help remain
  the three delivery mechanisms. No TypeDoc, no docs.rs publishing.
- No rewrite of `docs/business-use-cases.md` marketing copy. Its uncited
  quantitative claims are flagged as an open question (evidence or removal
  from primary navigation), not silently rewritten. `docs/roadmap.md` gets a
  factual status pass (shipped/active/deferred labels, correct crate names),
  not a strategy rewrite.
- No release-management policy changes. The CHANGELOG's 10-month untagged
  `[Unreleased]` backlog and the governance doc's unfulfilled commitments are
  flagged for maintainers, with only mechanical repairs in scope.
- Not chasing 100% rustdoc item coverage. In-code doc work targets module
  orientation (`//!`) and the highest-traffic public surfaces, not a
  `missing_docs` blanket.
- Operational changes to deployment selection. Removing or externalizing the
  `fastly.toml` `service_id` changes which service a deploy targets; it is
  an operationally owned follow-up with its own replacement plan, staging
  test, and rollback instructions, not part of this refresh.

## Work packages

All eight packages ship in the same single PR as this spec (#1049, branch
`spec-docs-refresh`): the spec commit lands first, then each package as one
commit (or a small commit series) in the order below, so the PR is
reviewable commit-by-commit. WP1 and WP2 are corrective, WP3-WP6 are
completion work, WP7-WP8 are quality and enforcement. (The reviewer
recommended splitting at least WP1 into its own PR for urgent publishing
containment; the single-PR delivery is a deliberate owner decision, recorded
in open question 7.)

### WP1: Publishing and policy hygiene

Smallest package, highest urgency.

- Add `srcExclude` to `docs/.vitepress/config.mts` covering
  `superpowers/**`, `internal/**`, `epics/**`, `guide/onboarding.md`, and
  `README.md`, and move `docs/guide/onboarding.md` to
  `docs/internal/onboarding.md` after scrubbing internal contacts, meeting,
  and access details. Exclusion from the build is not sufficient on its own:
  the repository is public, so source-sensitive details are scrubbed even
  from excluded files. Verify with a local `vitepress build` that the dist
  no longer contains those paths.
- Resolve `docs/public/CNAME`: it currently ships the placeholder
  `your-custom-domain.com` into every Pages deploy while `base` is set to
  `/trusted-server` (the two are mutually inconsistent). Default action:
  delete the CNAME and keep the project-path deploy; revisit if a custom
  domain is actually provisioned. Update `docs/README.md:138` and `:172`
  accordingly.
- `fastly.toml`: replace the personal email in `authors` with an empty list
  (matching the workspace `Cargo.toml`); add `# local test fixture, not a
real key` labels to the `[local_server]` secret/JWKS entries; add one-line
  comments to the four KV store declarations; remove the orphaned reference
  to the deleted `scripts/test-prebid-eids.sh` (line 38). The `service_id`
  removal is out of scope here (see Non-goals and open question 1).
- `docs/package.json`: set `"private": true` and align the license with the
  repository (Apache-2.0, currently ISC).
- `.github/pull_request_template.md`: fix `tracing` to `log` (line 40); add
  Cloudflare, Spin, and parity gates to the test-plan checkboxes.
- Fill `docs/guide/index.md` (currently 0 bytes, renders a blank page) with a
  guide landing page organized by reader journey (evaluator, operator,
  integrator, contributor), and retarget the top-nav Guide link
  (`config.mts:61`, currently `/guide/getting-started`) at it.
- Align `.claude/commands/{check-ci,verify,test-all,test-crate}.md` with the
  canonical gate list in `CLAUDE.md` (all currently omit Spin and
  `clippy-cloudflare-wasm`; `test-crate.md` uses an untargeted
  `cargo test -p`, the exact pattern `AGENTS.md` warns will fail). Add the
  missing Spin/cloudflare-wasm gates to `AGENTS.md`'s fallback list.

Acceptance: `vitepress build` output contains no `superpowers/`, `internal/`,
`epics/`, or onboarding pages; no real personal emails in tracked config; no
internal contacts or access instructions anywhere in the repo; every command
file lists the same gates as `CLAUDE.md`.

### WP2: Truth pass over existing content

Nothing new is written here beyond minimal replacement prose; the goal is
that nothing in the active sets is false. The pass starts from a complete
page inventory: every page in the active public set gets an explicit
disposition, verified, rewrite, or retire, recorded in the PR description.
Token greps establish that retired names are gone; they cannot validate
commands, APIs, auth, or behavior, so each "verified" disposition means the
page's commands and examples were actually checked against code.

- `docs/guide/api-reference.md`: delete `GET /first-party/ad` and
  `POST /third-party/ad` sections (endpoints do not exist). The full
  replacement reference is WP4; in this package, add a stub for
  `POST /auction` so the primary endpoint is not undocumented in the interim.
- Remove the same dead endpoints from `docs/guide/integrations-overview.md:46-48`,
  `docs/guide/error-reference.md:658`, and
  `docs/guide/integrations/prebid.md:515-531`.
- `docs/guide/error-reference.md`: remove or replace the nonexistent
  `npm run type-check` (line 597) and `--validate-config` (line 663)
  instructions with commands that exist.
- `docs/guide/key-rotation.md`: rewrite the Rust API examples against
  `core/src/request_signing/rotation.rs` (`KeyRotationManager::new` returns
  `Self`, not a `Result`) and add the Basic-auth requirement to the curl
  examples for admin endpoints.
- `docs/guide/proxy-signing.md`: full content review against
  `core/src/proxy.rs` signing (promoted from a follow-up; a
  security-relevant page cannot sit outside a documentation audit).
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
- Adapter support consistency: `docs/index.md:27`,
  `docs/guide/what-is-trusted-server.md:32`, `docs/roadmap.md:19,34-38`, and
  `docs/guide/architecture.md:154-159` currently give four different
  answers. Align all of them with the WP5 support matrix: Fastly production,
  Cloudflare deployable, Spin experimental (see Context item 4), Axum local
  development only. Fix `docs/roadmap.md:21-22` old crate names (`/common/`,
  `/cloudflare/`) and give roadmap line items shipped/active/deferred
  status labels.
- Retire `docs/guide/integrations/gam.md` and `kargo.md` (delete pages,
  remove sidebar entries). Neither integration exists; GAM ad serving is
  already covered factually via GPT/creative-opportunities docs. Before
  deletion, inventory inbound links (site-internal grep plus a GitHub search
  for the public URLs) and leave a client-side redirect stub for any
  previously published URL with known inbound references; `vitepress build`
  catches surviving internal links but not bookmarks or external links.
- Retire `FAQ_POC.md`: its headline answer ("NOT ready for use", two-partner
  Fastly+Equativ POC) is false on every axis. Delete it (git history
  preserves it); fold any still-true answers into
  `docs/guide/what-is-trusted-server.md`. Same inbound-link inventory as
  above before deletion.
- Remove the nonexistent `.with_asset(...)` builder method from
  `docs/guide/creative-processing.md:808` and
  `docs/guide/integration-guide.md:84,248`, replacing it with the real
  registration builder API (`with_proxy`, `with_head_injector`,
  `with_deferred_js`, ...). Closes #277. (The deeper integration-guide
  signature drift is fixed in WP5 by switching snippets to a compiling
  source.)
- `crates/trusted-server-core/src/auction/README.md`: point the route table
  at `crates/trusted-server-adapter-fastly/src/app.rs` and drop the invented
  line numbers (name the tables, `NAMED_ROUTES` / `routes_for_state()`,
  instead of line numbers so this cannot rot the same way); remove the
  `providers/your_provider.rs` instructions in favor of the real layout
  (`auction/provider.rs`, provider registration via
  `register_providers` in each integration); delete the APS `mock = true`
  sections (field no longer exists).
- `docs/guide/onboarding.md:51,107`: remove or retarget the two links to the
  nonexistent root `SEQUENCE.md` (as part of the WP1 move).
- `docs/epics/revenue-operations-dashboard.md`: correct its telemetry
  sections to reflect the shipped Tinybird pipeline (the epic proposes
  BigQuery/Grafana/Prometheus and predates it). It is excluded from the site
  by WP1 either way.
- `CHANGELOG.md` mechanical repairs: fix the two dead compare links (no
  `v1.2.0` tag exists), the `.rust-analyzer.json` reference (file does not
  exist), the retired `synthetic_id`/`x-synthetic-id` naming in the
  `[Unreleased]` entry (subsystem is now EC; shipped historical entries keep
  their original wording), section ordering per Keep-a-Changelog, the
  "fastly.tom" and "gogernance" typos, and add the missing entry for #992
  (DataDome IP exclusions and staging bypass, an operator-visible change).
- Environment files: document the two distinct configuration surfaces
  separately. (a) Runtime variables the server reads (Appendix D). (b) The
  `TRUSTED_SERVER__` typed overlay, which the runtime loader ignores but
  `ts config validate/diff/push` still applies when building the config
  blob (`crates/trusted-server-cli/tests/config_env_overlay.rs`). Repair
  both `.env.example` and `.env.dev` (both still carry retired
  `TRUSTED_SERVER__SYNTHETIC__*` keys), update
  `docs/guide/getting-started.md:74-77` (which tells users to `cp .env.dev
.env` and source it), and smoke-test the Axum quick start as written.
- `docs/guide/getting-started.md:141`: `[gdpr]` does not exist; the section
  is `[consent]`.

Acceptance: every active-public page has a recorded disposition; grepping
the active public and active repo sets (Source sets above; historical set
exempt) for `first-party/ad`, `third-party/ad`, `equativ`, `ad_servers`,
`RequestWrapper`, `trackImpression`, `SEQUENCE.md`, `synthetic_id` (outside
shipped changelog entries), `providers/your_provider`, `with_asset`,
`type-check`, and `mock = true` (APS context) returns nothing; no sidebar
entry points at a nonexistent integration.

### WP3: Configuration reference completion

Bring the two operator-facing config artifacts to parity with `Settings`
(`core/src/settings.rs:1916`, `#[serde(deny_unknown_fields)]`).

- `trusted-server.example.toml`: add commented, documented example blocks
  for the sections it lacks: `[tinybird]` (all 10 fields, with the note that
  `access_enabled` must remain false), `[consent]` (mode, expiration,
  jurisdiction, conflict resolution, `consent_store`), `[rewrite]`
  (`exclude_domains`, already referenced by a CHANGELOG breaking entry),
  `[tester_cookie]`, `[image_optimizer]` (`profile_sets` with one worked
  profile), `[[proxy.asset_routes]]` (a complete, valid route: `prefix`,
  `origin_url`, paired `path_pattern`/`target_path`, optional S3 SigV4 auth
  block), `[integrations.osano]`, the missing `[auction]` keys (`mediator`,
  `creative_store`; `allowed_context_keys` is already present at line 145),
  and `[debug].inject_adm_for_testing` with its never-in-production warning.
- `docs/guide/configuration.md`: add the missing `[consent]`, `[tinybird]`,
  and `[debug]` sections (the `[tester_cookie]`, `[rewrite]`, and
  image-optimizer sections already exist at lines 360, 702, and 946;
  verify their field lists rather than re-adding them); extend the
  Integration Configurations section from 5 to all 14 IDs (add `aps`,
  `datadome`, `didomi`, `sourcepoint`, `lockr`, `gpt`, `gpt_diagnostics`,
  `google_tag_manager`, `adserver_mock`), each with its typed config keys
  from the integration source.
- Every example block must actually parse: WP8 adds a test that feeds the
  uncommented example config through `Settings::from_toml`, so examples are
  finalized and validity-checked in CI rather than eyeballed.
- Add a parity checklist to the PR description mapping each of the 15
  `Settings` fields to its example-toml block and configuration.md heading
  (the table in Appendix B is the worklist).

Acceptance: every field of `Settings` appears in both
`trusted-server.example.toml` and `docs/guide/configuration.md`; every
integration ID accepted by deploy validation (`core/src/config.rs:29-44`) has
a config subsection; the WP8 example-parse test passes.

### WP4: API reference rebuild

Rebuild `docs/guide/api-reference.md` from the route inventory (Appendix A),
with per-endpoint contracts, not just paths.

- Document every named route: health, discovery/signing endpoints, admin key
  rotation (and the deliberately 404-denied legacy `/admin/keys/*` aliases),
  EC partner API (`/_ts/api/v1/batch-sync`, `/_ts/api/v1/identify`), tester
  cookie endpoints, `POST /auction`, `GET /_ts/page-bids` plus the legacy
  `/__ts/page-bids` alias, the four `/first-party/*` proxy endpoints,
  `/_ts/debug/ja4`, and the tsjs bundle endpoint (`/static/tsjs=...`,
  unified vs deferred vs standalone module forms with ETag behavior).
- Each endpoint follows a contract checklist: methods (including guarded
  ones, e.g. page-bids registers OPTIONS and denies it in-handler as a CORS
  preflight guard), auth requirement, request parameters/body schema,
  response codes and notable headers, cache/CORS behavior, the config gate
  that enables it, rate limits where present, and one example. The
  `/first-party/*` family gets explicit per-endpoint treatment: `/sign`
  mints short-lived signed URLs while `/proxy`, `/click`, and
  `/proxy-rebuild` validate different signed inputs; "tstoken signing" as a
  group label is not sufficient for a security-sensitive surface.
- Add an adapter availability and capability matrix: route availability per
  adapter (EC partner API, tester cookies, JA4 debug, and working key
  rotation are Fastly-only; Spin registers the canonical admin routes but
  returns unsupported responses; `/health` is absent on Cloudflare) plus the
  platform capabilities that differ per adapter (stores, geo, TTL storage,
  secrets, Tinybird sink construction, request filters), sourced from each
  adapter's `app.rs`, `main.rs`/`lib.rs`, and `platform.rs`.
- Document the fallback dispatch order (tsjs, integration proxy routes,
  asset routes, publisher origin proxy) and the fact that the publisher
  fallback registers seven explicit methods (GET, POST, HEAD, OPTIONS, PUT,
  PATCH, DELETE), so route-shadowing and method questions are answerable
  from docs.
- Add an Integration Endpoints section generated from each integration's
  `IntegrationProxy::routes()` registration (the integrations are enumerated
  in Appendix C) instead of today's three-entry list.

Acceptance: the route list in the reference matches the union of the four
adapter route tables with per-adapter availability flagged; every documented
route names its handler file and satisfies the contract checklist.

### WP5: New coverage pages and navigation repair

- Deployment docs with honest maturity labels, grouped under a new
  "Deployment" sidebar section alongside the existing (currently orphaned)
  `docs/guide/fastly.md`:
  - `docs/guide/cloudflare.md`: wrangler config, `TRUSTED_SERVER_KV`
    binding, `TRUSTED_SERVER_CONFIG` var with blob envelope, missing
    `/health`.
  - `docs/guide/axum-dev.md`: explicitly a local-development guide
    (env-var-backed stores, `PORT`, unsupported admin ops), not a
    deployment target.
  - `docs/guide/spin.md`: written only if the Spin runtime fix (follow-up
    issue below) lands first and a `spin up` smoke test proves non-health
    traffic works; otherwise the page is a short experimental-status notice
    describing the current limitation. The docs never present Spin as
    deployable while startup depends on the checked-in example config.
- A support matrix page (or architecture-page section) with owned columns:
  build status, intended use, runtime capability, operational support,
  known gaps, and release status per adapter. This matrix is the single
  source for every "runs on X" claim elsewhere (WP2 aligns existing pages
  to it).
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
- Integration guide integrity: make a compiling source the snippet origin.
  Either extract snippets from `testlight` (which registration, proxy, and
  TSJS tests already exercise) or add a doc-tested fixture integration; the
  guide's current hand-written snippets omit `RuntimeServices` from
  `IntegrationProxy::handle` and `proxy_request`, and import `fastly::http`
  in platform-neutral core code. Document `testlight` itself as the example
  integration in a named reference section of the guide.
- Extend the integration guide with the script-guard mechanism
  (`crates/trusted-server-js/lib/src/shared/script_guard.ts`, the
  per-integration guards such as `gpt/script_guard.ts` and
  `datadome/script_guard.ts`, and `shared/beacon_guard.ts`): what guards
  intercept, when an integration needs one, and how to add one. Today
  `script_guard` is mentioned only in `docs/guide/integrations/gpt.md`.
  Closes #341.
- Add `docs/guide/integrations/adserver_mock.md` for the mock auction
  provider used in local development, currently unmentioned in all docs.
- Extend `docs/guide/integrations/gpt.md` with the slot handoff mechanism
  (edge-injected `gpt_bootstrap.js`, the full shim takeover, targeting, APS
  renderer bridge, SPA hook); "handoff" currently appears nowhere in docs.
- `docs/guide/integrations-overview.md`: extend the comparison and
  performance tables from 7 to all 14 IDs using the registry capability
  matrix (Appendix C).
- Testing docs: rewrite root `TESTING.md` as the test-matrix index (the
  aliases from `.cargo/config.toml`, the seven `test.yml` jobs plus the four
  integration-test workflow jobs, the parity suite, `scripts/test-cli.sh`,
  integration/browser scripts, vitest). Its current auction runbook is
  verified against the running system and rewritten into
  `docs/guide/auction-testing.md` (cross-linked from the auction README),
  not mechanically moved: it contains fabricated log output and stale
  behavior claims. Update `docs/guide/testing.md` to cover
  cloudflare/spin/parity/CLI/browser suites and replace the fictional
  two-job CI YAML with the real seven-job layout.
- `docs/guide/cli.md`: full command reference from the clap tree (Appendix
  D), adding `ts config diff` and the `ts dev` subtree with its macOS-only
  gating, and linking to `ts-dev-proxy.md`.
- Site usability: enable VitePress `lastUpdated` (the deploy workflow
  already fetches full history for it) and local search
  (`themeConfig.search`), so the 1,600-line configuration reference is
  navigable; give mermaid diagrams a one-paragraph prose equivalent nearby.
- Navigation: add sidebar entries for the three orphaned real integrations
  (`gpt`, `google_tag_manager`, `sourcepoint`) and the new pages.
- `docs/guide/architecture.md`: describe all 10 workspace crates and the
  platform trait boundary; add the missing Cloudflare adapter section.

Acceptance: every integration ID is documented and nav-reachable (testlight
via its reference section in the integration guide); every adapter has a
guide or an honest status notice consistent with the support matrix; no real
page is orphaned; integration-guide snippets compile; `vitepress build`
passes (dead links fail the build).

### WP6: Root markdown and crate READMEs

Audit every existing root and crate document, not only the missing ones.

- `README.md`: current quick start including the `ts` CLI path
  (`ts config init` / `ts serve --adapter ...`) alongside `fastly compute
serve`; link the deployment guides; refresh the doc-site link table.
- `CONTRIBUTING.md` (untouched since 2026-01): reference the per-target
  alias system and full CI gate list, point to `CLAUDE.md`/`AGENTS.md` for
  agent workflows, fix the "could be dev/develop/master" boilerplate, and
  re-verify its error-handling guidance against current conventions.
- `CLAUDE.md` corrections beyond the CI gates section (WP8): it states the
  workspace default target is wasm32-wasip1; `.cargo/config.toml` sets no
  default target (per-target aliases and `Cargo.toml` `default-members` do
  that work). Re-verify its other build-system claims while there.
- `crates/trusted-server-integration-tests/README.md`: fix the wrong CI job
  name (line 231), the incomplete environments tree (lines 165-177), and
  the missing browser spec (lines 141-145) flagged in Appendix E.
- New crate READMEs (short, orientation-level: what it is, how it builds,
  where its docs live) for the seven crates lacking one:
  `trusted-server-adapter-fastly`, `-axum`, `-cloudflare`, `-spin`,
  `trusted-server-cli`, `trusted-server-js`, `trusted-server-openrtb-codegen`.
  Rewrite `crates/trusted-server-core/README.md` as an actual crate overview
  (currently covers 2 of ~40 modules), linking to the deep-dive docs. The
  Spin README carries the same experimental-status note as WP5.
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
every pre-existing root/crate document has a recorded
verified/rewritten/retired disposition; README quick start commands all run
against `main`.

### WP7: In-code documentation

Targeted, not exhaustive. The worklist below is the acceptance scope.

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
4. Module docs for the undocumented core files: `settings.rs`,
   `settings_data.rs`, `http_util.rs`, `proxy.rs`, `auth.rs`, `tsjs.rs`,
   `openrtb.rs`, `price_bucket.rs`, `rsc_flight.rs`, `host_rewrite.rs`,
   `storage/mod.rs`, `html_processor.rs` (expand the 3-line header for a
   1000-line streaming rewriter), `integrations/registry.rs`,
   `integrations/prebid.rs`, and the `nextjs/` and `datadome/` subtrees.
   (`test_support.rs` and `migration_guards.rs` are deliberately out of
   scope.)
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

Rustdoc verification commands (the exact matrix WP8 puts in CI):

- `cargo doc --no-deps -p trusted-server-core -p trusted-server-js -p trusted-server-openrtb --target wasm32-wasip1`
- `cargo doc --no-deps -p trusted-server-adapter-fastly --target wasm32-wasip1`
- `cargo doc --no-deps -p trusted-server-adapter-cloudflare --target wasm32-unknown-unknown --features cloudflare`
- `cargo doc --no-deps -p trusted-server-adapter-spin --target wasm32-wasip1 --features spin`
- `cargo doc --no-deps -p trusted-server-adapter-axum`
- `cargo doc --no-deps -p trusted-server-cli -p trusted-server-openrtb-codegen --target <host-triple>`

Acceptance: every item on the worklist above is complete; the rustdoc
command matrix builds warning-free with `RUSTDOCFLAGS="-D warnings"`; the
listed TypeScript files each have a file-header JSDoc block and every
`core/types.ts` export is documented (checked by the WP8 jsdoc lint scoped
to those files, or a grep count recorded in the PR description).

### WP8: Enforcement

Prevent recurrence. Two layers: build gates (links, rustdoc, doctests) and
semantic parity checks for every inventory the docs maintain by hand. All
additions are tests, scripts, and workflow steps; no runtime code changes.

Build gates:

- Docs site: add `npm run build` to the `format-docs` job in
  `.github/workflows/format.yml` so dead links fail PRs instead of the
  post-merge deploy. Align the two workflows' npm cache keys (one keys on
  `package.json`, the other on `package-lock.json`). Add `.tool-versions`
  to `deploy-docs.yml` trigger paths (the site renders versions from it, so
  version-only bumps must republish).
- Rustdoc: add a CI step running the WP7 command matrix with
  `RUSTDOCFLAGS="-D warnings"` (this denies
  `rustdoc::broken_intra_doc_links` by default). Do not enable
  `missing_docs`; the existing
  `missing_errors_doc`/`missing_panics_doc`/`doc_markdown` clippy trio plus
  `-D warnings` stays the item-level gate.
- Doctests: add a native-host `cargo test --doc -p trusted-server-core` step
  (doctests are silently skipped today because core is only tested
  cross-compiled).
- Add `[lints] workspace = true` to `trusted-server-openrtb-codegen`, the
  one crate not inheriting the doc lints.
- Dependabot: add the `github-actions` ecosystem and the Playwright
  `browser/package.json` npm root (both currently unmanaged).

Semantic parity checks (each catches a class of drift this audit found):

- Example-config validity: a unit test feeding the uncommented
  `trusted-server.example.toml` through `Settings::from_toml`, so every
  example block parses and passes validation (guards WP3 forever;
  `deny_unknown_fields` makes stale keys a hard failure).
- Route parity: a test per adapter asserting its registered route/method
  set matches a checked-in snapshot that the API reference is written from
  (guards Appendix A / WP4).
- CLI parity: a golden-file test of the rendered `ts` help tree (commands
  and flags) that `docs/guide/cli.md` is written against (guards WP5's CLI
  reference).
- Integration parity: a test asserting the registry's integration ID and
  capability set matches the checked-in table used by
  `integrations-overview.md` (guards Appendix C).
- Repo inventory: a CI script checking workspace members each have a
  README, every active public page is reachable from the sidebar or an
  explicit orphan allowlist, and the CI gate list in `CLAUDE.md` names the
  jobs that actually exist in the workflows.
- `CLAUDE.md` CI gates section: update to the real gate list (it omits
  ESLint, the CLI/codegen clippy jobs, the bench compile check, the release
  WASM builds, and the entire integration-tests workflow) so agents and the
  slash commands stay aligned with reality.
- Optional, decide at review: enable a minimal `jsdoc/*` ESLint rule set
  scoped to the WP7 TypeScript files; skipped by default to keep WP8
  low-noise.

Acceptance: a PR introducing a dead docs link, a broken intra-doc link, a
failing doctest, an invalid example-config block, or a route/CLI/integration
inventory change without a matching docs snapshot update fails CI.

## Sequencing and estimate

| Order | Package                  | Size | Depends on                        |
| ----- | ------------------------ | ---- | --------------------------------- |
| 1     | WP1 hygiene              | S    | -                                 |
| 2     | WP2 truth pass           | M    | -                                 |
| 3     | WP3 config reference     | M    | -                                 |
| 4     | WP4 API reference        | M    | WP2                               |
| 5     | WP5 new pages + nav      | L    | WP2 (nav), WP3 (links)            |
| 6     | WP6 root + crate READMEs | M    | -                                 |
| 7     | WP7 in-code docs         | M    | -                                 |
| 8     | WP8 enforcement          | M    | WP3, WP7 (gates must start green) |

Commits land in this order within the single PR, after the spec commit; WP8
comes last so the new CI gates turn green on the same PR.

## Verification

Before the PR is marked ready:
`cd docs && npm run lint && npm run format && npm run build`;
`cargo fmt --all -- --check`; the target-matched clippy/test aliases for any
crate whose source files changed (WP7, WP8 tests); the WP7 rustdoc command
matrix locally. The acceptance greps listed in WP2-WP4 are run over the
defined source sets and their output included in the PR description, along
with the WP2/WP6 page-disposition inventories and the WP3 parity checklist.
For WP1, a local `vitepress build` listing of `dist/` proves the exclusion
set. The Axum quick start from the updated getting-started guide is
smoke-tested as written.

## Open questions

1. `fastly.toml` `service_id`: removal is policy-correct and the CHANGELOG
   claims it already happened, but it changes deployment selection. Now an
   operationally owned follow-up (see Non-goals): needs an owner, a
   replacement mechanism, a non-production deployment test, and rollback
   instructions.
2. `docs/public/CNAME`: delete (recommended, matches the `/trusted-server`
   base path) or configure a real custom domain?
3. `FAQ_POC.md` and the `gam.md`/`kargo.md` pages: this spec recommends
   deletion with an inbound-link inventory and redirect stubs where
   referenced; confirm.
4. `docs/business-use-cases.md`: its quantitative claims need dated evidence
   and assumptions, or the page leaves primary navigation until verified.
   Which?
5. CHANGELOG: should a release be cut to drain the six breaking entries in
   `[Unreleased]`, or should the mechanical repairs land alone? (Mechanical
   repairs are in WP2 either way.)
6. Governance: who owns naming maintainers/CODEOWNERS and the meeting-minutes
   commitment? Out of scope here but flagged.
7. Delivery shape: the pre-implementation review recommends shipping WP1
   (publishing containment) as its own PR ahead of the rest; the current
   single-PR plan is the owner's explicit instruction. Confirm or split.

## Follow-up issues to file (code, not docs)

- Spin adapter builds runtime settings from the checked-in
  `trusted-server.example.toml` (`adapter-spin/src/app.rs:52`) and serves a
  blanket 503 on startup failure while `/health` returns 200
  (`adapter-spin/src/app.rs:404`). Blocking for the WP5 Spin deployment
  guide; until fixed, docs label Spin experimental. A `spin up` smoke test
  proving non-health traffic belongs to the fix's acceptance criteria.
- `ts --version` does not exist (no `#[command(version)]`).
- Vendored `edgezero-cli` help text leaks internal spec references
  ("5.4", "spec 3.3 Model A") into `ts config push --help`; fix upstream at
  the `edgezero` repo and bump the pinned tag.
- Tinybird access-log telemetry: config exists but is rejected at runtime;
  either wire it or remove the config surface.

## Appendix A: HTTP route inventory (truth source for WP4)

No single shared router exists; each adapter registers named routes plus a
publisher fallback. Fastly is the superset. Route tables:
`adapter-fastly/src/app.rs` (`NAMED_ROUTES`, `routes_for_state()`),
`adapter-axum/src/app.rs` (`named_routes()`), `adapter-cloudflare/src/app.rs`
(`build_router()`), `adapter-spin/src/app.rs` (`named_fallback_paths()`).
Adapter capability differences (stores, geo, TTL, secrets, Tinybird,
request filters) live in each adapter's `platform.rs` and entry point; WP4's
capability matrix is written from those files, not from this table alone.

| Route                                                                                         | Methods                                                                              | Availability                                                                            | Handler                                                                         |
| --------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------- |
| `/health`                                                                                     | GET                                                                                  | all except Cloudflare                                                                   | adapter entry points                                                            |
| `/_ts/debug/ja4`                                                                              | GET                                                                                  | Fastly only, gated by `debug.ja4_endpoint_enabled`                                      | `adapter-fastly/src/main.rs`                                                    |
| `/.well-known/trusted-server.json`                                                            | GET                                                                                  | all                                                                                     | `core/src/request_signing/endpoints.rs`                                         |
| `/verify-signature`                                                                           | POST                                                                                 | all                                                                                     | `core/src/request_signing/endpoints.rs`                                         |
| `/_ts/admin/keys/rotate`, `/_ts/admin/keys/deactivate`                                        | POST                                                                                 | Fastly working; Axum, Cloudflare, and Spin register the routes and return not-supported | `core/src/request_signing/endpoints.rs`, `adapter-fastly/src/management_api.rs` |
| `/admin/keys/*`                                                                               | the seven fallback methods                                                           | all: deliberately 404-denied legacy aliases                                             | adapter apps                                                                    |
| `/_ts/api/v1/batch-sync`                                                                      | POST                                                                                 | Fastly only; Bearer auth + rate limit                                                   | `core/src/ec/batch_sync.rs`                                                     |
| `/_ts/api/v1/identify`                                                                        | GET, OPTIONS                                                                         | Fastly only                                                                             | `core/src/ec/identify.rs`                                                       |
| `/_ts/set-tester`, `/_ts/clear-tester`                                                        | GET                                                                                  | Fastly only, gated by `tester_cookie.enabled`                                           | `core/src/tester_cookie.rs`                                                     |
| `/auction`                                                                                    | POST                                                                                 | all                                                                                     | `core/src/auction/endpoints.rs`                                                 |
| `/_ts/page-bids`                                                                              | GET; OPTIONS registered and denied in-handler (CORS preflight guard)                 | all; gated by `X-TSJS-Page-Bids` header                                                 | `core/src/publisher.rs`                                                         |
| `/__ts/page-bids`                                                                             | GET; OPTIONS registered and denied                                                   | legacy alias of `/_ts/page-bids`                                                        | `core/src/publisher.rs`                                                         |
| `/first-party/proxy`, `/first-party/click`, `/first-party/sign`, `/first-party/proxy-rebuild` | GET (sign/rebuild also POST)                                                         | all                                                                                     | `core/src/proxy.rs`                                                             |
| `/static/tsjs=<file>`                                                                         | GET                                                                                  | all (fallback chain)                                                                    | `core/src/publisher.rs` `handle_tsjs_dynamic`                                   |
| `/integrations/<id>/...`                                                                      | varies                                                                               | per enabled integration (Appendix C)                                                    | integration proxies                                                             |
| asset route prefixes                                                                          | GET, HEAD                                                                            | operator-configured `[[proxy.asset_routes]]`                                            | `core/src/proxy.rs` `handle_asset_proxy_request`                                |
| everything else                                                                               | the seven registered fallback methods (GET, POST, HEAD, OPTIONS, PUT, PATCH, DELETE) | publisher origin proxy + HTML rewriting                                                 | `core/src/publisher.rs` `handle_publisher_request`                              |

Fallback dispatch order: GPT-diagnostics request prep, EC state build and
integration request filters (DataDome may short-circuit), tsjs, integration
proxy routes, asset routes, publisher proxy.

## Appendix B: Settings sections (truth source for WP3)

From `core/src/settings.rs` (`Settings`, line ~1916). Columns record what
each artifact carries today.

| Section                    | Struct                        | `trusted-server.example.toml`                                                   | `configuration.md`         |
| -------------------------- | ----------------------------- | ------------------------------------------------------------------------------- | -------------------------- |
| `[publisher]`              | `Publisher`                   | present                                                                         | present                    |
| `[tester_cookie]`          | `TesterCookieConfig`          | missing                                                                         | present (line 360)         |
| `[ec]`                     | `Ec` + `EcPartner`            | present                                                                         | present                    |
| `[integrations.*]`         | per-integration typed configs | partial (osano missing)                                                         | 5 of 14 IDs                |
| `[[handlers]]`             | `Handler`                     | present                                                                         | present                    |
| `response_headers`         | map                           | present (commented)                                                             | present                    |
| `[request_signing]`        | `RequestSigning`              | present                                                                         | present                    |
| `[rewrite]`                | `Rewrite`                     | missing                                                                         | present (line 702)         |
| `[auction]`                | `AuctionConfig`               | missing `mediator`, `creative_store` (`allowed_context_keys` present, line 145) | present                    |
| `[consent]`                | `ConsentConfig`               | missing                                                                         | missing                    |
| `[proxy]`                  | `Proxy`                       | partial; `asset_routes` missing                                                 | present incl. asset routes |
| `[creative_opportunities]` | `CreativeOpportunitiesConfig` | present                                                                         | present                    |
| `[image_optimizer]`        | `ImageOptimizerSettings`      | missing                                                                         | present (line 946 area)    |
| `[tinybird]`               | `TinybirdSettings`            | missing                                                                         | missing                    |
| `[debug]`                  | `DebugConfig`                 | partial (`inject_adm_for_testing` missing)                                      | missing                    |

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

Runtime environment variables to document (WP2 `.env.example` / `.env.dev`):
`FASTLY_SERVICE_VERSION`, `FASTLY_IS_STAGING`, `FASTLY_HOSTNAME`,
`FASTLY_REGION`, `EDGEZERO_LOG_LEVEL`, `PORT` (Axum),
`TRUSTED_SERVER_CONFIG_{STORE}_{KEY}` / `TRUSTED_SERVER_SECRET_{STORE}_{KEY}`
(Axum stores), `TRUSTED_SERVER_CONFIG` (Cloudflare var), `EDGEZERO_*` store
overrides, and the build-time `TSJS_SKIP_BUILD`/`TSJS_TEST`.

Documented separately from the runtime variables: the `TRUSTED_SERVER__`
typed overlay, ignored by the runtime loader but applied by
`ts config validate/diff/push` when building the config blob
(`crates/trusted-server-cli/tests/config_env_overlay.rs`).

## Appendix E: Staleness finding index

Compact index of audit findings driving WP1/WP2; each was verified against
`main` at `2e85a1cdc`.

- Dead endpoints documented: `docs/guide/api-reference.md:85`
  (`/first-party/ad`), `:190` (`/third-party/ad`);
  `docs/guide/integrations-overview.md:46-48`;
  `docs/guide/error-reference.md:658`;
  `docs/guide/integrations/prebid.md:515-531`.
- Dead operator commands: `docs/guide/error-reference.md:597`
  (`npm run type-check`), `:663` (`--validate-config`).
- Obsolete API examples: `docs/guide/key-rotation.md:301-310`
  (`KeyRotationManager::new(...)?`; constructor returns `Self`),
  unauthenticated admin curl examples.
- Fabricated content: `docs/guide/ad-serving.md:11-18,43,48,77-83` (Equativ,
  `[ad_servers]`, `trackImpression`); `docs/guide/architecture.md:97-104`
  (`RequestWrapper`); `docs/guide/integration-guide.md:313` (equativ bidder).
- Integration-guide snippets that do not compile:
  `integration-guide.md:96` (`handle` without `RuntimeServices`, vs
  `registry.rs:282-288`), `:132` (`proxy_request` without `services`, vs
  `proxy.rs:737-742`), `:134` (`use fastly::http` in core-neutral code).
- Wrong config names: `docs/guide/getting-started.md:141` (`[gdpr]`).
- Nonexistent builder method: `.with_asset(...)` in
  `docs/guide/creative-processing.md:808`,
  `docs/guide/integration-guide.md:84,248` (issue #277).
- Script-guard mechanism absent from the integration guide (issue #341).
- Old crate layout: `docs/roadmap.md:21-22` (the only surviving instance).
- Adapter support contradictions: `docs/index.md:27`,
  `docs/guide/what-is-trusted-server.md:32`, `docs/roadmap.md:19,34-38` vs
  `docs/guide/architecture.md:154-159`; Axum described as a deployment
  target; Spin described as production-capable despite
  `adapter-spin/src/app.rs:52` (settings from the checked-in example toml)
  and `:404` (blanket 503 on startup failure).
- Aspirational sidebar pages: `docs/guide/integrations/gam.md` (no such
  integration, "Q1 2026" passed), `kargo.md`.
- Auction README: route table file/line rot, nonexistent `providers/` dir,
  removed APS `mock` key (`crates/trusted-server-core/src/auction/README.md:269-285,466-473,487-489,543-549,577`).
- Dead links: `docs/guide/onboarding.md:51,107` (`SEQUENCE.md`);
  `CHANGELOG.md:51-52` (no `v1.2.0` tag).
- CHANGELOG: retired `synthetic_id` naming in `[Unreleased]` (`:24`),
  nonexistent `.rust-analyzer.json` (`:51`), missing #992 entry, section
  order, typos.
- Environment files: `.env.example` and `.env.dev` both carry retired
  `TRUSTED_SERVER__SYNTHETIC__*` keys; `docs/guide/getting-started.md:74-77`
  tells users to copy and source `.env.dev`.
- Integration-tests README: wrong CI job name (`:231`), missing environment
  files (`:165-177`), missing browser spec (`:141-145`).
- fastly.toml: personal email (`:4`), service id (`:10`, ops-owned
  follow-up), orphaned script reference (`:38`), unlabeled key fixtures
  (`:48-74`).
- Publishing: 75 `docs/superpowers/**` files built into the public site (no
  `srcExclude`); `docs/guide/onboarding.md` published with internal
  contacts; `docs/public/CNAME` placeholder; empty `docs/guide/index.md`;
  nav Guide link bypasses the landing page (`config.mts:61`);
  `docs/package.json` not private, ISC license in an Apache-2.0 repo.
- Root-doc drift: `CLAUDE.md:102` (no workspace default target exists);
  `CONTRIBUTING.md` stale since 2026-01.
- Slash-command drift: `.claude/commands/{check-ci,verify,test-all}.md` omit
  Spin/cloudflare-wasm/parity gates; `test-crate.md` untargeted `cargo test`.
- Tooling: no `cargo doc` in CI; doctests never run (cross-compile only);
  `format-docs` never runs `vitepress build`; `deploy-docs.yml` not
  triggered by `.tool-versions` changes; `eslint-plugin-jsdoc` inert;
  `openrtb-codegen` missing `[lints] workspace = true`; PR template says
  `tracing`; no semantic parity checks for routes, config, CLI,
  integrations, crates, navigation, or CI gates.
