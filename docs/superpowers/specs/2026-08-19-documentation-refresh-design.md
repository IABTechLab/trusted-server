# Documentation Refresh (Full Surface)

**Date:** 2026-08-19
**Revised:** 2026-08-21 (round 5; addresses all five pre-implementation reviews)
**Status:** Draft, pending review
**Scope:** Documentation and doc tooling. No runtime behavior changes.
Baseline audited at `main` commit `2e85a1cdc` (2026-08-18); realigned and
rebased 2026-08-20 onto release branch `rc/202608` at `d4cd2cc82`, which is
the PR target and the truth source for every claim in this spec. After any
further rebase, the inventories in the appendices are re-verified against
the new merge base before implementation continues. The latest six rc
commits (APS native rendering, DataDome staging-requirement removal, APS
creative frame scrollbars) touch documented behavior and are explicitly
re-checked in WP2. Line citations are from the main baseline unless marked
rc; spot-rechecked claims cite rc line numbers.

## Context

Trusted Server's documentation spans four surfaces: the VitePress site
(`docs/`), root and per-crate markdown, in-code documentation (rustdoc, clap
help, JSDoc), and configuration templates (`trusted-server.example.toml`,
`fastly.toml`, `edgezero.toml`, `.env.example`, `.env.dev`). A four-track
audit of `main`, hardened by five pre-implementation reviews, found systemic
drift in every surface. The failures fall into six categories:

1. **Fabricated or dead content presented as real.** The API reference
   documents `GET /first-party/ad` and `POST /third-party/ad`; neither exists
   anywhere in `crates/` (the real client auction endpoint is `POST /auction`).
   On rc the dead sections sit at `api-reference.md:86,191` with two more
   occurrences at `:707,711`, and the same dead endpoints recur in
   `docs/guide/error-reference.md:658` and
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
   `--validate-config` flag (`error-reference.md:663`);
   `docs/guide/key-rotation.md:301-310` shows an obsolete
   `KeyRotationManager::new(...)?` signature (the real constructor returns
   `Self`, not a `Result`); and `docs/guide/configuration.md:1954-1957` (rc)
   shows a Rust example importing a nonexistent
   `settings_data::get_settings` (the exported loader is
   `get_settings_from_config_store`) and using `println!`, which the repo's
   own conventions forbid.
2. **Incomplete references.** `docs/guide/configuration.md` has no
   `[consent]`, `[tinybird]`, or `[debug]` sections (rc added a `[cache]`
   section; those three remain missing), and its Integration
   Configurations section covers 5 of 14 IDs, with the existing five never
   re-audited (the Prebid implementation exposes valid keys the reference
   omits). `trusted-server.example.toml`
   has no `[tinybird]`, `[consent]`, `[rewrite]`, `[tester_cookie]`, or
   `[image_optimizer]` blocks. The API reference omits `POST /auction`,
   `/_ts/page-bids`, `/health`, `/_ts/debug/ja4`, the EC partner API, and all
   integration endpoints except three. `docs/guide/cli.md` gained
   `config diff`, `ts dev proxy`, and the ad-template workflows on rc, but
   does not cover the new lifecycle commands (`active-version`,
   `healthcheck`, `rollback`) or `config gc`.
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
   (`build_state()` in `adapter-spin/src/app.rs`, line 58 on rc), and a
   startup failure installs a router that returns 503 for all traffic
   while `/health` still returns 200 (`startup_error_router()`, line 420
   on rc). Unstable Rust files are cited by symbol; line numbers are
   hints. No smoke test proves non-health traffic
   works under `spin up`. CI compiles the Spin artifact; compilation is not
   evidence of production maturity. Capability differences are also
   documented nowhere: asset-route dispatch, integration request filters,
   the image optimizer, and Tinybird auction telemetry exist only in the
   Fastly adapter today; the other adapters construct no telemetry sink and
   silently use the no-op default.
5. **Publishing and policy hygiene.** All 120 internal spec/plan markdown files under
   `docs/superpowers/` are built and published to the public GitHub Pages site
   (no `srcExclude` in `docs/.vitepress/config.mts`), along with
   `docs/guide/onboarding.md` (internal contacts, meetings, access guidance),
   an internal epic, and an ops runbook. `docs/public/CNAME` contains the
   literal placeholder `your-custom-domain.com`. `fastly.toml` carries a real
   personal email (`authors`, line 4) and a real Fastly service id (line 10)
   against the repo's own sensitive-data policy, and unlabeled base64 key
   fixtures that read as credentials. `docs/package.json` is not `private`
   and declares an ISC license in an Apache-2.0 repository. The maintained
   agent instructions under `.claude/agents/` are badly stale: they still
   describe a three-crate Fastly-only workspace, cite the nonexistent
   `RequestWrapper` trait, list outdated verification gates, and assume all
   PRs target `main`. Active examples
   violate the fictional-data policy beyond that: `ec-setup-guide.md:15`
   names a real deployment domain, and `.env.example` uses non-reserved
   `publisher.com` values instead of `.example` domains.
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
add enforcement, including executable parity checks that are bound to the
reader-facing markdown, so the same drift is caught by CI instead of by the
next manual audit. Every claim in the refreshed docs must be verifiable
against code at the PR HEAD's merge base with `rc/202608` (the August 2026
release); anything aspirational must be labeled as such or removed; adapter
support claims must come from an honest, owned support matrix rather than
marketing copy.

The source-of-truth map:

| Artifact                   | Truth source                                                                                                                                                                                                                                          | Consumers               |
| -------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------- |
| HTTP API reference         | Adapter route tables and entry points (`adapter-*/src/app.rs`, `adapter-*/src/main.rs`, `adapter-*/src/platform.rs`) + core handlers                                                                                                                  | Publishers, partners    |
| Config reference           | `Settings` in `crates/trusted-server-core/src/settings.rs` plus the typed per-integration config structs (the root uses `deny_unknown_fields`; `IntegrationSettings` is a flattened map, so integration blocks need their own direct deserialization) | Operators               |
| CLI reference              | The built `ts` binary's recursive `--help` tree (clap definitions in `crates/trusted-server-cli/src/run.rs` and command modules, plus flags owned by the lockfile-resolved `edgezero-cli`)                                                            | Operators               |
| Integration pages          | Three inventories, tested separately: registry `builders()` (13 registrations), the auction-provider inventory (`auction/mod.rs` `provider_builders()`, which adds `adserver_mock`), and the JS module registry (`JS_ALWAYS` adds `creative`)         | Publishers, integrators |
| Integration guide snippets | A compiling sample integration (`testlight` or a doc-tested fixture), never hand-written pseudo-code                                                                                                                                                  | Integrators             |
| Deployment guides          | `edgezero.toml` adapter blocks + per-adapter manifests + adapter support matrix                                                                                                                                                                       | Operators               |
| Architecture               | `Cargo.toml` workspace members + `core/src/platform/`                                                                                                                                                                                                 | Contributors            |
| Test/CI docs               | `.cargo/config.toml` aliases + `.github/workflows/*`                                                                                                                                                                                                  | Contributors            |

## Source sets

Truth-pass acceptance criteria and parity checks operate on defined source
sets, not "all tracked files" (the refresh spec itself, and archived specs
under `docs/superpowers/`, legitimately contain every retired term):

- **Active public set:** everything VitePress builds, i.e. `docs/**`
  excluding the WP1 `srcExclude` list. This is what site visitors see.
- **Active repo set:** root markdown (`README.md`, `CONTRIBUTING.md`,
  `TESTING.md`, `CHANGELOG.md`, `FAQ_POC.md` until it is actually
  retired, `ProjectGovernance.md`, `AGENTS.md`,
  `CLAUDE.md`), crate READMEs, config templates
  (`trusted-server.example.toml`, `fastly.toml`, `edgezero.toml`,
  `.env.example`, `.env.dev`), and `.claude/commands/*.md`.
- **Active maintained internal set:** documents that are neither public-site
  pages nor historical artifacts but are still maintained and must pass the
  truth standard: `docs/README.md`, `docs/internal/**` (including the moved
  onboarding page), `scripts/README.md` and `tinybird/README.md` once
  created, `.claude/skills/**` (operator-facing skills such as the
  Fastly deployment skill), `.claude/agents/**` (maintained agent
  instructions), and `.github/pull_request_template.md`.
- **Historical set:** `docs/superpowers/**` (specs, plans, implementation
  notes, archive) and shipped `CHANGELOG.md` release entries. Exempt from
  retired-term greps; a changelog entry describing a rename may name the
  old identifier.

## Goals

- Every endpoint, config key, command, flag, crate name, and code path named
  in active-set documentation exists in the code at the PR target
  (`rc/202608`), with adapter-specific availability stated where behavior
  differs.
- Every shipped, operator- or publisher-visible surface has documentation:
  all 14 integration IDs, all 16 config sections, the deployment adapters
  (with honest maturity labels), all `ts` commands, the telemetry pipeline,
  and the tsjs module system.
- The adapter support model is truthful: three deployment adapters (Fastly
  production; Cloudflare; Spin, currently experimental) plus the Axum
  local-development adapter, backed by a published support matrix.
- The public docs site publishes only intended pages, and the containment
  actually reaches the published site (see WP1: Pages deploys only from
  `main`). Internal specs, plans, epics, onboarding, and runbooks are
  excluded from the build, and internal-only details are scrubbed from
  anything that stays in the public repository regardless of whether
  VitePress builds it.
- Sensitive real-world values are removed from source-controlled config, and
  examples use fictional data (reserved `.example` domains, clearly fake
  credentials) per the repo policy in `CLAUDE.md`. One narrowly scoped
  exception is recognized: the `fastly.toml` `service_id`, whose removal
  is an operational migration (open question 1). It is recorded in the
  scanner allowlist as owner-approved and time-bounded, with rationale
  and a review date, so the goal and the scanner acceptance agree.
- CI gates catch documentation regressions: docs build (dead internal links)
  on PRs, rustdoc build with broken-intra-doc-link denial, doctests actually
  running, and executable parity checks bound to the reader-facing markdown.
- Root markdown (`README`, `CONTRIBUTING`, `TESTING`, `CHANGELOG`) accurately
  describes the current workspace, build system, and test matrix.

## Non-goals

- No changes to runtime behavior, routes, config schema, or code structure,
  with one boundary clarification: parity checks added by WP8 may add tests
  and scripts, but not alter runtime code. Code defects the audit exposed
  (Spin's hardcoded example-config startup, Tinybird
  access logging config present but not wired, internal spec references
  leaking into vendored `edgezero-cli` help text) are tracked as follow-up
  issues; the Spin one blocks publishing a Spin deployment guide (WP5).
- No new documentation toolchains. VitePress, rustdoc, and clap help remain
  the three delivery mechanisms. No TypeDoc, no docs.rs publishing.
- No rewrite of `docs/business-use-cases.md` marketing copy. Default
  handling hardened after review: the page is excluded from the built site
  (`srcExclude`) until every quantitative claim carries dated evidence and
  unshipped features are visibly labeled - nav-only removal would leave a
  known-false page published and locally searchable, and would violate
  WP5's no-orphan acceptance (the page presents planned headless-browser
  malvertising detection as shipped while the roadmap calls it planned).
  Open question 4 records the alternative of an evidence-based rewrite in
  this pass. While it remains in the repository unpublished, the source
  file carries a prominent top banner stating it is unverified and
  excluded from the site. `docs/roadmap.md` gets a
  factual status pass (shipped/active/deferred labels, correct crate names),
  not a strategy rewrite.
- No release-management policy changes. The CHANGELOG's 10-month untagged
  `[Unreleased]` backlog and the governance doc's unfulfilled commitments are
  flagged for maintainers, with only mechanical repairs in scope.
- No dedicated accessibility audit gate. The site uses the stock VitePress
  theme with no custom interactive components; WP5 adds the pieces with
  direct accessibility value (prose equivalents for mermaid diagrams,
  local search, `lastUpdated` context), and anything beyond that
  (keyboard/contrast/screen-reader smoke checks) is deliberately deferred
  until the site carries custom components that need it.
- Not chasing 100% rustdoc item coverage. In-code doc work targets module
  orientation (`//!`) and the highest-traffic public surfaces, not a
  `missing_docs` blanket.
- Operational changes to deployment selection. Removing or externalizing the
  `fastly.toml` `service_id` changes which service a deploy targets; it is
  an operationally owned follow-up with its own replacement plan, staging
  test, and rollback instructions, not part of this refresh.

## Delivery shape

The owner's standing instruction is one PR (#1049, branch
`spec-docs-refresh`, targeting `rc/202608`) carrying the spec plus all eight
work packages, one commit (or small series) per package, reviewable
commit-by-commit. The second review surfaced a mechanical constraint that
forces one exception: GitHub Pages deploys only on pushes to `main`
(`deploy-docs.yml`), so publishing containment merged to rc does not reach
the live site until rc merges to main. Therefore:

- The WP1 publishing-containment subset ships as a minimal separate PR
  straight to `main` so the exposure closes immediately. Its contents are
  exactly: the `srcExclude` change (covering `superpowers/**`,
  `internal/**`, `epics/**`, `guide/onboarding.md`, `README.md`, and
  `business-use-cases.md`), the onboarding move/scrub, the filled Guide
  landing page (`docs/guide/index.md` is empty today, and the post-deploy
  smoke asserts it), and the navigation edits that removing pages forces:
  retarget the top-nav Guide link at the landing page and drop the
  Business Value nav item. Removing every link to an excluded source is a
  containment invariant - a build that navigates to excluded pages fails.
  Nothing else - neither the CNAME resolution nor the marketing-page
  content disposition blocks or rides in it. The rc PR carries the same
  changes; the rc→main merge reconciles to an identical state.
- Everything else lands only in the single rc PR.
- CodeQL today analyzes only PRs targeting `main`, so the rc PR carrying
  new workflows, generators, and scripts would go unanalyzed. Decision:
  WP8 adds `rc/*` to `codeql.yml`'s PR branch triggers, and CodeQL joins
  the final gate list in Verification.

Open question 7 asks the owner to confirm this shape.

## Work packages

WP1 and WP2 are corrective, WP3-WP6 are completion work, WP7-WP8 are quality
and enforcement. Commits land in the order below.

### WP1: Publishing and policy hygiene

Smallest package, highest urgency. The containment subset also ships to
`main` directly (see Delivery shape).

- Add `srcExclude` to `docs/.vitepress/config.mts` covering
  `superpowers/**`, `internal/**`, `epics/**`, `guide/onboarding.md`,
  `README.md`, and `business-use-cases.md` (per the Non-goals default),
  and move `docs/guide/onboarding.md` to
  `docs/internal/onboarding.md` after scrubbing internal contacts, meeting,
  and access details. Exclusion from the build is not sufficient on its own:
  the repository is public, so source-sensitive details are scrubbed even
  from excluded files. Verify with a local `vitepress build` that the dist
  no longer contains those paths.
- The containment pieces are independent of the CNAME decision and must
  not wait for it: `srcExclude` and the onboarding move/scrub ship
  immediately; the CNAME change follows its own resolution (open question 2) in a separate commit.
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

Acceptance: `vitepress build` output contains no `superpowers/`,
`internal/`, `epics/`, onboarding, or business-use-cases pages; the
containment PR to `main` is merged, the live site no longer serves those
URLs, and a positive post-deploy smoke passes (site root, the Guide
landing page, and one reference page return 200 with expected content);
the rollback procedure is documented in the containment PR (owner: the
maintainer driving this refresh) and treats the exclusions and scrubbing
as non-rollbackable security invariants: failure-prone cosmetic changes
(CNAME, navigation) live in separate commits, and recovery means
reverting only the causal non-security commit or redeploying a known-good
artifact that retains the exclusion and scrub - never republishing the
excluded material; no real
personal emails in tracked config; no internal contacts or access
instructions anywhere in the repo; every command file lists the same gates
as `CLAUDE.md`.

### WP2: Truth pass over existing content

Nothing new is written here beyond minimal replacement prose; the goal is
that nothing in the active sets is false. The pass starts from a complete
page inventory: every page in the active public and active maintained
internal sets gets an explicit disposition, verified, rewrite, or retire.
The inventory is checked into the repository under
`docs/internal/audits/` (inside the active maintained internal set, not
the exempt historical tree), stamped with the audited merge-base SHA, with
per-page source anchors, not left in a PR description. The inventory is an
audit record of this pass; the WP8 parity gates, not the inventory, are
the continuing control. Token greps establish that retired names are
gone; they cannot validate commands, APIs, auth, or behavior, so each
"verified" disposition means the page's commands and examples were actually
checked against code, and executable fences are governed by the WP8
snippet manifest: every nonempty fence in every non-historical set gets a
checked-in disposition (graded modes per WP8, from compile and
typed-validation down to expiring manual waivers), and CI fails on new
fences with no classification.

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
- `docs/guide/configuration.md:1954-1957` (rc): fix the loader example
  (`settings_data::get_settings` does not exist; the exported function is
  `get_settings_from_config_store`) and remove the `println!` usage the
  repo's conventions forbid.
- `docs/guide/key-rotation.md`: rewrite the Rust API examples against
  `core/src/request_signing/rotation.rs` (`KeyRotationManager::new` returns
  `Self`, not a `Result`) and add the Basic-auth requirement to the curl
  examples for admin endpoints.
- `docs/guide/proxy-signing.md`: full content review against
  `core/src/proxy.rs` signing (promoted from a follow-up; a
  security-relevant page cannot sit outside a documentation audit).
- Fictional-data policy audit over the active public, active repo, and
  active maintained internal sets: replace the real deployment domain in
  `ec-setup-guide.md:15` and the non-reserved `publisher.com` values in
  `.env.example` with reserved `.example` domains and clearly fictional
  values; sweep both sets for other real domains, customer names, or
  credential-shaped strings. Reviewed canonical vendor endpoints (e.g. real
  GPT/DataDome CDN hosts an integration genuinely proxies) stay, everything
  else becomes fictional. All exceptions live in ONE typed allowlist
  schema - entry categories: vendor URL, exact-path/hash-pinned fake
  credential fixture (e.g. the `fastly.toml` local JWKS material),
  historical example, service ID - each entry carrying owner, rationale,
  and expiry/review date; the scanner rejects expired or orphaned
  entries. The scanner and allowlist scaffolding land in WP8a so this
  pass can use them. `CLAUDE.md`'s example-domains-only policy gains a
  sentence describing the exception model (WP6 makes that edit).
- Re-verify the pages touched by the final six rc commits:
  `docs/guide/integrations/datadome.md` (the staging requirement was
  removed from protection behavior in the same commit that rewrote the
  page; confirm prose and code now agree) and
  `docs/guide/integrations/aps.md` (native rendering mode landed; confirm
  the page describes the current render modes and the conditional proxy).
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
- Retire `docs/guide/integrations/gam.md` and `kargo.md`: the markdown
  files are REPLACED with tombstone content (successor link, canonical
  metadata, optional meta refresh) so every previously published route
  keeps serving, unconditionally - inbound discovery cannot find
  bookmarks or unindexed links; a client-side stub is not an HTTP
  redirect and is not claimed to be one. Sidebar entries are removed;
  old-route smoke tests assert the tombstones serve. Neither integration
  exists; GAM ad serving is already covered factually via
  GPT/creative-opportunities docs.
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
.env` and source it), and smoke-test the Axum quick start against the
  contract defined in Verification.
- `docs/guide/getting-started.md:141`: `[gdpr]` does not exist; the section
  is `[consent]`.

Acceptance: the checked-in inventory covers every page in the active public
and active maintained internal sets with a disposition and source anchors;
grepping the active sets (historical set exempt) for `first-party/ad`,
`third-party/ad`, `equativ`, `ad_servers`, `RequestWrapper`,
`trackImpression`, `SEQUENCE.md`, `synthetic_id` (outside shipped changelog
entries), `providers/your_provider`, `with_asset`, `type-check`,
`settings_data::get_settings`, and `mock = true` (APS context) returns
nothing; the fictional-data sweep finds no unreviewed real domains or
credential-shaped values; no sidebar entry points at a nonexistent
integration.

### WP3: Configuration reference completion

Bring the two operator-facing config artifacts to parity with `Settings`
(`core/src/settings.rs`, 16 fields on rc, `#[serde(deny_unknown_fields)]` at
the root) and with the typed per-integration configs, which the root parse
does NOT validate: `IntegrationSettings` is a flattened map, and disabled
integrations can skip typed deserialization entirely.

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
  and `[debug].inject_adm_for_testing` with its never-in-production warning
  (rc's `[debug]` block now carries `ja4_endpoint_enabled`,
  `auction_html_comment`, and `auction_html_comment_options`). For the rc
  `[cache]` section, promote the commented `[[cache.asset_rules]]` examples
  to a complete worked block covered by the WP8 example harness.
- Field-path inventories, exact implementation (decided): a Serde-aware
  AST extractor - a new dev-only tool crate, `tools/docs-parity`
  (host-target, outside workspace default-members, the only place the new
  `syn` dependency lives, so production crates and their dependency
  closure are untouched), run as
  `cargo run -p docs-parity -- check|generate` with a JSON output schema
  checked into the tool. It parses the config struct definitions and
  their serde attributes - field-level (`rename`, `alias`, `default`,
  `flatten`, `skip`, `skip_deserializing`, `deserialize_with`) AND
  container/variant-level (`rename_all`, `deny_unknown_fields`, `tag`,
  `content`, `untagged`), since renamed/tagged enums change accepted
  values - into the machine inventory, and FAILS CLOSED on any
  shape-changing serde attribute it does not recognize. A serializer walk is
  explicitly rejected: it cannot see deserialization-only aliases
  (`pub_id` in `aps.rs`, `s3_sig_v4` in `settings.rs`), custom
  `deserialize_with` shapes, defaulted/`Option` fields, the flattened
  `IntegrationSettings` map, or `serde(skip)` implementation fields. What
  the AST cannot decide (the accepted shapes of each `deserialize_with`,
  dynamic map-key grammars, tagged-enum representations) lives in a
  checked companion manifest the extractor requires an entry for, so an
  unannotated custom deserializer fails CI. The inventory distinguishes
  canonical keys (documented) from accepted-but-deprecated aliases
  (listed as aliases, never as primary documentation), and records how
  flattened and dynamic-key forms render in the reference. The chain is
  Rust serde surface to machine inventory to template to generated
  markdown, checked in both directions: a newly added field breaks CI
  until inventory, template, and reference are updated. Reconcile
  `docs/guide/configuration.md`'s field tables against that inventory. This audits the five existing integration
  sections (Prebid's reference is already missing valid keys) as well as
  adding the nine absent ones (`aps`, `datadome`, `didomi`, `sourcepoint`,
  `lockr`, `gpt`, `gpt_diagnostics`, `google_tag_manager`, `adserver_mock`).
- `docs/guide/configuration.md`: add the missing `[consent]`, `[tinybird]`,
  and `[debug]` sections (the `[tester_cookie]`, `[rewrite]`, and
  image-optimizer sections already exist at lines 360, 702, and 946;
  reconcile their field lists against the inventory rather than re-adding
  them).
- Every example block must actually validate. WP8 builds the harness this
  package relies on: placeholder-substituting parse of the full template,
  plus marker-extracted parses of each commented example block and direct
  typed deserialization of each integration example (bypassing the
  disabled-integration short-circuit).
- Add a parity checklist to the PR description mapping each of the 16
  `Settings` fields to its example-toml block and configuration.md heading
  (the table in Appendix B is the worklist).

Acceptance: every field path in the inventory appears in both
`trusted-server.example.toml` (as a real or commented example) and
`docs/guide/configuration.md`; every integration ID accepted by deploy
validation has a config subsection whose field table matches its struct;
the WP8 example harness passes.

### WP4: API reference rebuild

Rebuild `docs/guide/api-reference.md` from the route inventory (Appendix A),
with per-endpoint contracts, not just paths, and with per-adapter accuracy.

- Document every named route: health, discovery/signing endpoints, admin key
  rotation (and the deliberately 404-denied legacy `/admin/keys/*` aliases),
  the rc admin diagnostics (`GET /_ts/admin/ec` and `/_ts/admin/ec/{id}`,
  registered on all four adapters but functional only on Fastly, which has
  the EC identity KV store - the others return not-supported, matching the
  key-rotation pattern; `GET /_ts/admin/eids`, a request-inspection
  handler that works on all four adapters),
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
- Document per-adapter request pipelines rather than one generalized
  pipeline: integration request filters (DataDome) run pre-route and exist
  only in the Fastly adapter today; asset-route dispatch and the image
  optimizer are Fastly-only; Tinybird auction emission is Fastly-only (the
  other adapters construct no sink and use the no-op default). The Fastly
  fallback order is tsjs, integration proxy routes, asset routes, publisher
  origin proxy; the other adapters dispatch tsjs, integration proxy routes,
  publisher proxy.
- Add an adapter availability and capability matrix: route availability per
  adapter (EC partner API, tester cookies, JA4 debug, admin EC KV lookups,
  and working key rotation are Fastly-only; Spin registers the canonical
  admin key routes but returns unsupported responses; `/health` is absent on
  Cloudflare) plus the platform capabilities that differ per adapter
  (stores, geo, TTL storage, secrets, Tinybird sink construction, request
  filters, asset routes, image optimizer), sourced from each adapter's
  `app.rs`, `main.rs`/`lib.rs`, and `platform.rs`.
- The publisher fallback registers seven explicit methods (GET, POST, HEAD,
  OPTIONS, PUT, PATCH, DELETE); document that set rather than "all methods".
- Add an Integration Endpoints section generated from each integration's
  `IntegrationProxy::routes()` registration (the integrations are enumerated
  in Appendix C) instead of today's three-entry list.

Acceptance: the route list in the reference matches the union of the four
adapter route tables, with per-adapter availability flagged; every documented
route names its handler file and satisfies the contract checklist; the WP8
route snapshots (which include response semantics, not just method and
path) agree with the published tables.

### WP5: New coverage pages and navigation repair

- Deployment docs with honest maturity labels, grouped under a new
  "Deployment" sidebar section alongside the existing (currently orphaned)
  `docs/guide/fastly.md`:
  - `docs/guide/cloudflare.md`: wrangler config, `TRUSTED_SERVER_KV`
    binding, `TRUSTED_SERVER_CONFIG` var with blob envelope, missing
    `/health`, no asset routes/filters/telemetry (per the capability
    matrix).
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
  (auth/build/serve/deploy/provision, plus the rc additions
  `active-version`, `healthcheck`, `rollback`). Fold the still-relevant
  parts of `docs/internal/EDGEZERO_MIGRATION.md` in; the internal runbook
  itself stays excluded from the site.
- New `docs/guide/telemetry.md`: auction telemetry from
  `[tinybird]` config through `auction_sink_from_settings` to the
  `tinybird/` datasources, pipes, and rollups; the operator setup path
  (Tinybird tokens in `ts_secrets`); explicit notes that emission is
  Fastly-only today and that access-log telemetry is not yet wired
  (`access_enabled` must remain false). New `tinybird/README.md` covering
  the `tb` workflow and file layout.
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
  performance tables from 7 to all 14 IDs using the three-inventory
  capability data (Appendix C), including APS's actual shape (head injector
  always, auction provider, proxy conditional on rendering mode).
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
- `docs/guide/cli.md`: full command reference generated from the built
  binary's recursive help tree (Appendix D). rc already covers
  `config diff`, `ts dev proxy`, `audit generate`, and the ad-template
  workflows; add the missing lifecycle commands (`active-version`,
  `healthcheck`, `rollback`) and `config gc`, verify the rc additions
  against the help tree, and link to `ts-dev-proxy.md`.
- Site usability: enable VitePress `lastUpdated` (the deploy workflow
  already fetches full history for it) and local search
  (`themeConfig.search`).
- Information architecture: today Configuration and CLI sit under a
  "Development" sidebar group; restructure navigation into Operator,
  Deployment, and Reference groups alongside the developer material, add
  a generated section index at the top of the ~2,000-line configuration
  reference (preserving URLs and anchors), and include the four reader
  journeys (evaluator, local developer, operator, integration author) as
  explicit acceptance walks.
- Release identity: add a global banner stating the site is rolling
  documentation of the unreleased `main` line. Provenance is mechanical:
  the Pages build injects `GITHUB_SHA` (local builds use
  `git rev-parse HEAD`), the banner links the exact build SHA, and the
  deploy smoke asserts the built HTML contains it; the content-audit
  baseline SHA (recorded in the audit inventory) is a distinct value and
  labeled as such. No versioned-docs machinery exists (Pages publishes one `main`
  build; the blob envelope is an integrity check, not a schema/version
  handshake), so the banner promises none: per-release documentation, if
  ever wanted, is a separately designed follow-up. Compatibility guidance
  stays factual: upgrade the binary before pushing configs that carry new
  fields, per the CHANGELOG rollback notes.
- Diagram accessibility: inventory the active public mermaid diagrams and
  give each a nearby one-paragraph prose equivalent; the inventory with a
  per-diagram checkbox is part of WP5's recorded acceptance.
- Navigation: add sidebar entries for the three orphaned real integrations
  (`gpt`, `google_tag_manager`, `sourcepoint`) and the new pages;
  `business-use-cases` is excluded from the build per the Non-goals
  default (open question 4), so it neither sits in navigation nor counts
  against the no-orphan acceptance.
- `docs/guide/architecture.md`: describe all 10 workspace crates and the
  platform trait boundary; add the missing Cloudflare adapter section.

Acceptance: every integration ID is documented and nav-reachable (testlight
via its reference section in the integration guide); every adapter has a
guide or an honest status notice consistent with the support matrix; no real
page is orphaned; integration-guide snippets compile; `vitepress build`
passes (dead internal links fail the build); local search returns results
for a sampled query set; the rolling-main banner renders on every page;
the mermaid inventory shows a checked prose equivalent for every diagram;
the four reader-journey walks are recorded.

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
- New `scripts/README.md` (one line per script) and `tinybird/README.md`
  (WP5); both join the active maintained internal set and the WP2
  disposition inventory.
- `.claude/skills/**`: audit the operator-facing skills (including the
  Fastly deployment skill) against current commands and config, same truth
  standard as the command files.
- Human-facing workflow comments join the maintained truth set. Two
  known-false comments are repaired: `.github/workflows/test.yml` (Spin
  release-build comment claims environment overrides make the artifact
  boot with usable settings, but the Spin adapter loads the checked-in
  example TOML directly; and the `test-cli` comment claims a workspace
  default target that `.cargo/config.toml` does not set - the mechanism
  is `Cargo.toml` `default-members`).
- `.claude/agents/**`: audit every agent instruction file; they currently
  describe a three-crate Fastly-only workspace, cite the nonexistent
  `RequestWrapper` trait (`code-architect.md:11`, `repo-explorer.md:12`),
  omit Cloudflare/Spin/parity gates (`verify-app.md:19`), and assume PRs
  target `main` (`pr-creator.md:177`).
- `CLAUDE.md` policy edits owned here: the vendor-endpoint exception
  sentence (WP2) and the `# Examples` standard reconciliation (WP7).
- `ProjectGovernance.md`: the two claims contradicted by repo state
  (meeting minutes "maintained within the repository" - none exist;
  "continuous releases" - none tagged since v1.1.0) become accurate
  statements of intent, unless open question 6 resolves them differently;
  link it from `CONTRIBUTING.md` so the governance model is
  visible at the contribution point. Naming maintainers/CODEOWNERS is a
  maintainer decision, flagged as an open question.
- Add `readme = "README.md"` to each crate's `Cargo.toml` once the READMEs
  exist.

Acceptance: every workspace package reported by `cargo metadata` has a
README (the metadata-based WP8 check is authoritative);
every pre-existing root/crate/skill document has a recorded
verified/rewritten/retired disposition; README quick start commands all run
against the PR HEAD.

### WP7: In-code documentation

Targeted, not exhaustive. The worklist below is the acceptance scope.

1. `core/src/lib.rs` module index: currently lists 12 of 37 public modules
   and links a `test_support` module; make it complete and grouped
   (identity, consent, auction, HTML pipeline, proxy, platform, config).
2. `core/src/platform/` (on rc: 4 of 10 files carry `//!` - `mod.rs`,
   `image_optimizer.rs`, `template_assembly.rs`, `template_cache.rs`):
   module docs for `traits.rs`, `types.rs`, `kv.rs`, `http.rs`,
   `error.rs`; the test-only module stays excluded. This is the
   cross-adapter contract and the highest-value rustdoc gap in the repo.
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

Style follows `CLAUDE.md` documentation standards with one deliberate
divergence that WP6 reconciles: `CLAUDE.md` currently mandates
`# Examples` on every public API function, which no part of the codebase
satisfies; the standard is updated to require examples where they compile
as doctests and earn their keep (`redacted.rs` is the model), so the two
documents state the same rule. This spec does not attempt examples on all
~589 public functions.

Rustdoc verification commands (the exact matrix WP8 puts in CI; the CI job
needs pinned Node/npm setup because documenting `trusted-server-js` runs its
npm-based build script):

- `cargo doc --no-deps -p trusted-server-core -p trusted-server-js -p trusted-server-openrtb --target wasm32-wasip1`
- `cargo doc --no-deps -p trusted-server-adapter-fastly --target wasm32-wasip1`
- `cargo doc --no-deps -p trusted-server-adapter-cloudflare --target wasm32-unknown-unknown --features cloudflare`
- `cargo doc --no-deps -p trusted-server-adapter-spin --target wasm32-wasip1 --features spin`
- `cargo doc --no-deps -p trusted-server-adapter-axum`
- `cargo doc --no-deps -p trusted-server-cli -p trusted-server-openrtb-codegen --target x86_64-unknown-linux-gnu` (CI; locally substitute the host triple, e.g. `aarch64-apple-darwin`)

Acceptance: every item on the worklist above is complete; the rustdoc
command matrix builds warning-free with `RUSTDOCFLAGS="-D warnings"`; the
listed TypeScript files each have a file-header JSDoc block and every
`core/types.ts` export is documented, enforced by the mandatory WP8 jsdoc
lint scoped to those files.

### WP8: Enforcement

Prevent recurrence. Delivered in two stages so content work can depend on
it: WP8a (scaffolding) lands FIRST, immediately after WP1 - the AST
extractor and config inventory, the example-config harness, the snippet
manifest tooling, the capability record, the route/CLI golden tooling, and
the generated-region generator - so WP3-WP5 write against working
generators instead of retrofitting them at the end; WP8b (gate
activation: wiring everything into CI as blocking checks, workflow edits,
Dependabot, CodeQL, link checks) lands last so gates turn green on the
same PR. Two layers: build gates (links, rustdoc, doctests) and semantic
parity checks. Crucially, the parity checks are bound to the
reader-facing markdown, not only to snapshots a contributor could update
while leaving the prose stale: the reference tables in
`api-reference.md`, `cli.md`, `configuration.md`, and
`integrations-overview.md` live inside delimited generated regions
(`<!-- generated:BEGIN ... END -->`) produced from the machine-readable
inventories, and CI fails when regenerating them produces a diff. All
additions are tests, scripts, and workflow steps; no runtime code changes.

Build gates:

- Docs site: add `npm run build` to the `format-docs` job in
  `.github/workflows/format.yml` so dead internal links fail PRs instead of
  the post-merge deploy. External links are out of the PR gate; the decided policy is a weekly
  scheduled link-check workflow with concrete mechanics: a pinned checker
  version, `issues: write` scoped to that job only, deduplication against
  the existing open issue, auto-close on recovery, a named owner (the
  maintainer driving this refresh) with a best-effort SLA, and a
  fixture-based test of the reporter before it merges. Normalize every `setup-node` cache key across all workflows to the
  relevant `package-lock.json` (today one keys on `package.json`, another
  on the lockfile). Add `.tool-versions`
  to `deploy-docs.yml` trigger paths (the site renders versions from it, so
  version-only bumps must republish). Add `rc/*` to `codeql.yml` PR branch
  triggers (decided in Delivery shape; not conditional). Whether `push`
  events on release branches should also be analyzed is a separate
  maintainer choice recorded in open question 8.
- Rustdoc: add a CI step running the WP7 command matrix with
  `RUSTDOCFLAGS="-D warnings"` (this denies
  `rustdoc::broken_intra_doc_links` by default), with pinned Node per the
  WP7 note. Do not enable `missing_docs`; the existing
  `missing_errors_doc`/`missing_panics_doc`/`doc_markdown` clippy trio plus
  `-D warnings` stays the item-level gate.
- Doctests: add a native-host `cargo test --doc -p trusted-server-core` step
  (doctests are silently skipped today because core is only tested
  cross-compiled). This job needs the same pinned Node/npm setup as the
  rustdoc job: core depends on `trusted-server-js`, whose build script
  invokes npm.
- Add `[lints] workspace = true` to `trusted-server-openrtb-codegen`, the
  one crate not inheriting the doc lints.
- Dependency governance: Dependabot gains the `github-actions` ecosystem,
  the Playwright `browser/package.json` npm root, and the Next.js fixture
  npm root (all currently unmanaged). Pin the Wrangler version used in
  CI/docs instead of installing latest; state the tested Spin and Tinybird
  CLI versions (or compatibility ranges) in the deployment/telemetry
  guides.

Semantic parity checks (each catches a class of drift this audit found):

- Example-config harness (replaces the naive parse test, which cannot pass:
  `Settings` finalization deliberately rejects the template's placeholder
  admin password, and TOML parsing ignores commented blocks). The harness
  (a) applies a deterministic substitution of the known placeholders and
  deliberately invalid disabled-block values (e.g. empty IDs) with
  synthetic valid values, and asserts the substituted template fully
  parses and finalizes; (b) groups the template's `[integrations.*]`
  tables by their first-segment integration ID (nested tables such as
  `[integrations.prebid.bundle]` are part of their parent's subtree, not
  standalone configs), and for each of the 14 IDs deserializes the
  complete subtree into its typed config struct with ignored-key
  detection (several structs, including Permutive's, do not reject
  unknown fields) and runs `Validate::validate`; because the runtime
  path (`Settings::get_typed` and the deploy checks that delegate
  through it) deliberately returns `None` for disabled integrations, the
  harness then constructs an isolated `Settings` fixture per integration
  with that integration forced enabled (14 named fixtures, with any
  inter-integration dependencies stated explicitly) and runs the real
  deploy/startup validation from `core/src/config.rs` against it; the
  same treatment applies to marker-delimited commented example blocks;
  and (c) separately asserts the distributed template still contains the
  placeholder markers, so a template that would deploy without
  customization fails CI.
- Route parity: a test per adapter asserting its registered route set,
  methods, and response semantics/status for guarded routes match the
  machine-readable inventory that feeds the api-reference generated
  regions. Route definitions expose only path, methods, and handler, so
  the generated regions cover the route/availability tables; the
  per-endpoint contract prose (auth, schemas, headers, cache/CORS, config
  gates, rate limits) is explicitly manually owned, marked as such in the
  page, and backed by targeted tests where they exist
  (`Settings::ADMIN_ENDPOINTS` coverage, config-gate behavior tests)
  rather than falsely claimed as generated. The adapter capability matrix
  rows are likewise either tied to a per-adapter test or marked manually
  owned.
- CLI parity: golden files of the built `ts` binary's recursive `--help`
  tree on both Linux and macOS (the `ts dev` subtree is compile-time
  gated to macOS, and CI already runs the CLI suite on both hosts),
  merged into a platform-annotated union (including the dependency-owned
  `edgezero-cli` lifecycle flags at the locked version) that feeds the
  cli.md generated region. The generated projection is defined: command
  and flag names, argument shapes, and defaults are generated verbatim;
  description text passes through the same retired/internal-term gate as
  prose, and descriptions that fail it (the vendored help currently
  leaks internal spec references like "5.4" and "spec 3.3 Model A") are
  replaced from a checked description-override table until the upstream
  fix lands, so known-internal strings are never published.
- Integration parity: a checked capability record, keyed by stable
  integration/provider ID, is the single source that both the parity
  tests assert against the three inventories (registry `builders()`,
  auction `provider_builders()`, JS module registry including
  `JS_ALWAYS`) and the integrations-overview generated region renders
  from. Conditions (APS's proxy conditional on rendering mode, DataDome's
  request filter conditional on protection) use a small typed grammar -
  capability, config predicate - not prose, and a fixture matrix
  evaluates every condition in both states; the parity test requires set
  equality against each inventory, not subset containment. This record exists because no single registry API is
  sufficient: `IntegrationMetadata` omits HTML post-processors and JS
  loading modes, and `provider_builders()` is a private list of bare
  function pointers without stable IDs.
- Config parity: the field-path inventories from WP3 feed the
  configuration.md field tables' generated regions.
- Snippet manifest: a checked-in manifest classifying every nonempty
  fence in every non-historical set (active public, active repo, and
  active maintained internal - root READMEs, TESTING.md, agent and
  command files included), all languages, not only the four the audit
  started from (the active public pages alone carry ~30 HTML, ~13 HTTP,
  ~8 JS/TS, ~5 CSS, and 1 YAML fence). Modes are graded to actually catch
  the failures this audit found: shell fences distinguish syntax-only
  (`bash -n`) from command/flag-existence and help/dry-run checks, which
  are required for operator instructions; config fences use typed/schema
  validation via the WP3 inventory, not bare TOML/JSON parsing; Rust
  fences compile; JS/TS and YAML fences parse (typecheck where cheap);
  HTTP fences are checked against the route inventory (method and path
  must exist); HTML/CSS fences get structural checks or explicit manual
  waivers. Manual waivers are not an open escape hatch: each
  carries owner, reason, expiry/review date, and source anchor, and CI
  fails on expired waivers and on unclassified new fences.
- Domain/credential scanner: a deterministic scan for secrets, PII,
  customer domains, and customer identifiers over ALL tracked source
  files - the historical set is exempt from retired-term greps, not from
  privacy scanning (archived docs already carry publisher-specific
  identifiers, e.g. the 2026-03-24 publisher-ID audit). Allowlist-aware
  (the WP2 vendor allowlist, plus reviewed historical exceptions), with
  negative fixtures proving it fails on planted values. Anything the scan
  finds is scrubbed from the current tree; whether a finding warrants
  credential rotation or history rewriting is escalated to the
  maintainer as a per-finding decision, recorded in the audit inventory.
- Repo inventory: a CI script checking workspace members each have a
  README (via `cargo metadata`, the authoritative package list, not a
  `find` over directories) and every active public page is reachable from
  the sidebar or an explicit orphan allowlist.
- Gate manifest: one checked manifest of the canonical CI gates, compared
  against the workflows AND against every human-facing copy - `CLAUDE.md`,
  `AGENTS.md`, `.claude/commands/*.md`, and the PR template - so WP8b's
  own gate additions cannot silently invalidate WP1's alignment of those
  same files at the final commit.
- `CLAUDE.md` CI gates section: update to the real gate list (it omits
  ESLint, the CLI/codegen clippy jobs, the bench compile check, the release
  WASM builds, and the entire integration-tests workflow), regenerated
  from the gate manifest above together with `AGENTS.md`, the command
  files, and the PR template, so all four surfaces change in the same
  commit.
- A scoped `jsdoc/*` ESLint rule set over the WP7 TypeScript files is
  mandatory (a PR-description grep count provides no recurrence
  protection); the plugin is already installed with zero rules enabled.

Acceptance, in two explicitly separated classes. Executable regression
fixtures - every runtime gate is exercised by at least one negative
fixture proving it fails on the regression it exists to catch: a dead
internal docs link, a broken intra-doc link, a failing doctest, an invalid
or unknown-keyed example-config block (including a disabled integration
table), a planted non-allowlisted domain or credential-shaped string, an
unclassified or expired-waiver snippet fence, a missing JSDoc block in a
WP7-scoped file, a route/CLI/config/integration inventory change without
the matching regenerated markdown region (including a macOS-only CLI
divergence), a missing crate README or unlisted orphan page, a gate-list
mismatch in any of the four human-facing surfaces, and a removed
manual-ownership marker. Static configuration assertions - checked once
in review with the evidence linked in the PR description, not fixtures:
CodeQL branch triggers, normalized cache keys, Dependabot roots, pinned
Wrangler/checker versions. Regenerating all generated regions and both
CLI goldens at the final PR HEAD produces no diff; the scheduled link
reporter's fixture test passes.

## Sequencing and estimate

| Order | Package                                     | Size | Depends on                                                  |
| ----- | ------------------------------------------- | ---- | ----------------------------------------------------------- |
| 0     | WP1 containment subset → separate `main` PR | XS   | -                                                           |
| 1     | WP1 hygiene (full, in rc PR)                | S    | -                                                           |
| 2     | WP8a enforcement scaffolding                | M    | - (extractor, harness, manifests, goldens, generators)      |
| 3     | WP2 truth pass                              | M    | WP8a (snippet manifest)                                     |
| 4     | WP3 config reference                        | M    | WP8a (inventory + harness)                                  |
| 5     | WP4 API reference                           | M    | WP2, WP8a (route inventory + regions)                       |
| 6     | WP5 new pages + nav                         | L    | WP2 (nav), WP3 (links), WP8a (CLI union, capability record) |
| 7     | WP6 root + crate READMEs                    | M    | -                                                           |
| 8     | WP7 in-code docs                            | M    | -                                                           |
| 9     | WP8b gate activation                        | M    | WP2-WP7 (all gates must start green)                        |

Commits land in this order within the single rc PR, after the spec commit;
WP8b comes last so the new CI gates turn green on the same PR (WP8a is
deliberately early).

## Verification

Before the rc PR is marked ready, at its final HEAD:

- All applicable GitHub checks green, explicitly including: CodeQL (with
  `rc/*` added to its PR triggers), format
  (fmt/clippy matrix, ESLint, Prettier for js and docs), the seven `test.yml`
  jobs (rust/axum/cloudflare/spin/parity/cli/typescript), the four
  integration-test workflow jobs (including browser), the release WASM
  builds, and the JS build (`node build-all.mjs`) and test
  (`npx vitest run`) suites for the TypeScript/MJS files WP7 touches.
- The new WP8 parity tests and scripts run green, and regenerating every
  generated markdown region produces no diff.
- `cd docs && npm run lint && npm run format && npm run build`.
- The WP7 rustdoc command matrix locally with `RUSTDOCFLAGS="-D warnings"`.
- The acceptance greps from WP2-WP4 over the defined source sets, output
  recorded in the PR description alongside the WP3 parity checklist; the
  page-disposition inventory is checked in (WP2).
- For WP1, a local `vitepress build` listing of `dist/` proves the exclusion
  set, and the separate `main` containment PR is merged (live-site URLs
  return 404).
- The Axum quick start smoke test with a defined first-success contract:
  starting from the updated getting-started instructions with a canonical
  config, the server starts, `GET /health` returns 200 `ok`, and one
  representative publisher-proxy request against a local stub origin
  returns the expected rewritten HTML; the run and cleanup steps are
  recorded in the PR description.

## Open questions

Owner for all: the repo maintainer driving this refresh. Each question
blocks the named package; none blocks starting WP2-WP7 content work except
where stated.

1. `fastly.toml` `service_id` (ops-owned follow-up): the allowlist entry
   that lets the scanner pass requires an owner and review date up front,
   so naming that owner blocks WP8a's scanner activation (not content
   work); the migration itself (replacement mechanism, non-production
   deployment test, rollback instructions) blocks nothing here.
2. `docs/public/CNAME` (blocks only its own follow-up commit, never the
   containment PR). Both branches are specified: delete (recommended,
   matches the `/trusted-server` base path; smoke re-runs against project
   URLs), or configure a real custom domain, which requires `base: '/'`,
   Pages custom-domain + DNS + TLS configuration, and canonical-URL and
   asset-URL smoke tests before it ships.
3. `FAQ_POC.md` and the `gam.md`/`kargo.md` pages (blocks their WP2
   retirements): this spec recommends retiring them, with the gam/kargo
   routes unconditionally preserved as tombstones. If deletion of
   `FAQ_POC.md` is rejected, the defined fallback is archival under the
   historical tree or a factual rewrite - it does not silently stay; it
   remains in the active repo set until one of those happens.
4. `docs/business-use-cases.md` (does not block containment - the default
   exclusion ships in it): default is exclusion from the built site until
   quantitative claims carry dated evidence and unshipped features are
   labeled; the alternative is an evidence-based rewrite in this pass.
5. CHANGELOG (blocks nothing): release management stays out of scope
   entirely - cutting a release to drain the seven breaking `[Unreleased]`
   entries is a maintainer decision outside this project. The
   deterministic WP2 edit assumes no release: keep the `[1.2.0]` section
   with an explicit "(tag v1.2.0 was never published)" annotation, repoint
   the link references to resolvable compares, and leave entries
   untouched. If a release lands externally before this PR merges, the
   branch rebases and re-audits rather than absorbing release work.
6. Governance ownership (blocks the WP6 governance edit only): who owns
   naming maintainers/CODEOWNERS and the meeting-minutes commitment?
7. Delivery shape (blocks starting implementation): confirm the shape in
   "Delivery shape": one rc PR for everything, plus the minimal
   publishing-containment PR to `main` that the Pages deploy trigger makes
   necessary.
8. CodeQL `push` coverage for release branches (blocks nothing; PR-trigger
   coverage is already decided): should `push` events on `rc/*` also be
   analyzed?

## Follow-up issues to file (code, not docs)

- Spin adapter builds runtime settings from the checked-in
  `trusted-server.example.toml` (`build_state()` in
  `adapter-spin/src/app.rs`) and serves a blanket 503 on startup failure
  while `/health` returns 200 (`startup_error_router()`). Blocking for
  the WP5 Spin deployment
  guide; until fixed, docs label Spin experimental. A `spin up` smoke test
  proving non-health traffic belongs to the fix's acceptance criteria.
- Vendored `edgezero-cli` help text leaks internal spec references
  ("5.4", "spec 3.3 Model A") into `ts config push --help`; fix upstream at
  the `edgezero` repo and bump the pinned tag.
- Tinybird access-log telemetry: config exists but is rejected at runtime;
  either wire it or remove the config surface. Auction emission is also
  Fastly-only; wiring the sink in other adapters is a code decision to
  file, not a docs gap.

## Appendix A: HTTP route inventory (truth source for WP4)

No single shared router exists; each adapter registers named routes plus a
publisher fallback. Fastly is the superset. Route tables:
`adapter-fastly/src/app.rs` (`NAMED_ROUTES`, `routes_for_state()`),
`adapter-axum/src/app.rs` (`named_routes()`), `adapter-cloudflare/src/app.rs`
(`build_router()`), `adapter-spin/src/app.rs` (`named_fallback_paths()`).
Adapter capability differences (stores, geo, TTL, secrets, Tinybird,
request filters, asset routes, image optimizer) live in each adapter's
`platform.rs` and entry point; WP4's capability matrix is written from those
files, not from this table alone. WP8's route snapshots also record response
semantics/status for guarded and unsupported routes, not only method and
path.

| Route                                                                                         | Methods                                                                              | Availability                                                                                                                                                                                                           | Handler                                                                                                                                                                                                   |
| --------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `/health`                                                                                     | GET                                                                                  | all except Cloudflare                                                                                                                                                                                                  | adapter entry points                                                                                                                                                                                      |
| `/_ts/debug/ja4`                                                                              | GET                                                                                  | Fastly only, gated by `debug.ja4_endpoint_enabled`                                                                                                                                                                     | `adapter-fastly/src/main.rs`                                                                                                                                                                              |
| `/.well-known/trusted-server.json`                                                            | GET                                                                                  | all                                                                                                                                                                                                                    | `core/src/request_signing/endpoints.rs`                                                                                                                                                                   |
| `/verify-signature`                                                                           | POST                                                                                 | all                                                                                                                                                                                                                    | `core/src/request_signing/endpoints.rs`                                                                                                                                                                   |
| `/_ts/admin/keys/rotate`, `/_ts/admin/keys/deactivate`                                        | POST                                                                                 | Fastly working; Axum, Cloudflare, and Spin register the routes and return not-supported                                                                                                                                | `core/src/request_signing/endpoints.rs`, `adapter-fastly/src/management_api.rs`                                                                                                                           |
| `/_ts/admin/ec`, `/_ts/admin/ec/{id}`                                                         | GET                                                                                  | registered on all four adapters; functional only on Fastly (EC identity KV store), others return not-supported (`adapter-axum/src/app.rs:330`, `adapter-cloudflare/src/app.rs:500`, `adapter-spin/src/app.rs:800`, rc) | `core/src/ec/admin.rs`                                                                                                                                                                                    |
| `/_ts/admin/eids`                                                                             | GET                                                                                  | all four adapters (request-inspection handler); Basic-auth gated                                                                                                                                                       | `core/src/ec/admin.rs`; registrations in `adapter-axum/src/app.rs:342`, `adapter-cloudflare/src/app.rs:506`, `adapter-spin/src/app.rs:802` (rc; lines are hints, cite `admin_eids_handler` registrations) |
| `/admin/keys/*`                                                                               | the seven fallback methods                                                           | all: deliberately 404-denied legacy aliases                                                                                                                                                                            | adapter apps                                                                                                                                                                                              |
| `/_ts/api/v1/batch-sync`                                                                      | POST                                                                                 | Fastly only; Bearer auth + rate limit                                                                                                                                                                                  | `core/src/ec/batch_sync.rs`                                                                                                                                                                               |
| `/_ts/api/v1/identify`                                                                        | GET, OPTIONS                                                                         | Fastly only                                                                                                                                                                                                            | `core/src/ec/identify.rs`                                                                                                                                                                                 |
| `/_ts/set-tester`, `/_ts/clear-tester`                                                        | GET                                                                                  | Fastly only, gated by `tester_cookie.enabled`                                                                                                                                                                          | `core/src/tester_cookie.rs`                                                                                                                                                                               |
| `/auction`                                                                                    | POST                                                                                 | all                                                                                                                                                                                                                    | `core/src/auction/endpoints.rs`                                                                                                                                                                           |
| `/_ts/page-bids`                                                                              | GET; OPTIONS registered and denied in-handler (CORS preflight guard)                 | all; gated by `X-TSJS-Page-Bids` header                                                                                                                                                                                | `core/src/publisher.rs`                                                                                                                                                                                   |
| `/__ts/page-bids`                                                                             | GET; OPTIONS registered and denied                                                   | legacy alias of `/_ts/page-bids`                                                                                                                                                                                       | `core/src/publisher.rs`                                                                                                                                                                                   |
| `/first-party/proxy`, `/first-party/click`, `/first-party/sign`, `/first-party/proxy-rebuild` | GET (sign/rebuild also POST)                                                         | all                                                                                                                                                                                                                    | `core/src/proxy.rs`                                                                                                                                                                                       |
| `/static/tsjs=<file>`                                                                         | GET                                                                                  | all (fallback chain)                                                                                                                                                                                                   | `core/src/publisher.rs` `handle_tsjs_dynamic`                                                                                                                                                             |
| `/integrations/<id>/...`                                                                      | varies                                                                               | per enabled integration (Appendix C)                                                                                                                                                                                   | integration proxies                                                                                                                                                                                       |
| asset route prefixes                                                                          | GET, HEAD                                                                            | Fastly only today; operator-configured `[[proxy.asset_routes]]`                                                                                                                                                        | `core/src/proxy.rs` `handle_asset_proxy_request`, dispatched from `adapter-fastly/src/app.rs`                                                                                                             |
| everything else                                                                               | the seven registered fallback methods (GET, POST, HEAD, OPTIONS, PUT, PATCH, DELETE) | publisher origin proxy + HTML rewriting                                                                                                                                                                                | `core/src/publisher.rs` `handle_publisher_request`                                                                                                                                                        |

Per-adapter pipelines: Fastly runs pre-route integration request filters
(DataDome), then dispatches tsjs, integration proxy routes, asset routes,
publisher proxy. Axum, Cloudflare, and Spin have no request filters or
asset-route dispatch today: tsjs, integration proxy routes, publisher
proxy.

## Appendix B: Settings sections (truth source for WP3)

From `core/src/settings.rs` (`Settings`; 16 fields on rc/202608). Columns
record what each artifact carries today. On rc, `request_signing` and
`creative_opportunities` are `Option` fields. Root parsing does not
validate integration blocks (flattened map) or commented examples; the WP8
harness covers both.

| Section                    | Struct                                 | `trusted-server.example.toml`                                                        | `configuration.md`                   |
| -------------------------- | -------------------------------------- | ------------------------------------------------------------------------------------ | ------------------------------------ |
| `[publisher]`              | `Publisher`                            | present                                                                              | present                              |
| `[tester_cookie]`          | `TesterCookieConfig`                   | missing                                                                              | present (line 360)                   |
| `[ec]`                     | `Ec` + `EcPartner`                     | present                                                                              | present                              |
| `[integrations.*]`         | per-integration typed configs          | partial (osano missing)                                                              | 5 of 14 IDs; existing five unaudited |
| `[[handlers]]`             | `Handler`                              | present                                                                              | present                              |
| `response_headers`         | map                                    | present (commented)                                                                  | present                              |
| `[request_signing]`        | `RequestSigning` (Option)              | present                                                                              | present                              |
| `[rewrite]`                | `Rewrite`                              | missing                                                                              | present (line 702)                   |
| `[auction]`                | `AuctionConfig`                        | missing `mediator`, `creative_store` (`allowed_context_keys` present, line 145)      | present                              |
| `[consent]`                | `ConsentConfig`                        | missing                                                                              | missing                              |
| `[cache]` (rc)             | `CacheSettings`                        | commented `[[cache.asset_rules]]` examples only                                      | present (rc)                         |
| `[proxy]`                  | `Proxy`                                | partial; `asset_routes` missing                                                      | present incl. asset routes           |
| `[creative_opportunities]` | `CreativeOpportunitiesConfig` (Option) | present                                                                              | present                              |
| `[image_optimizer]`        | `ImageOptimizerSettings`               | missing                                                                              | present (line 946 area)              |
| `[tinybird]`               | `TinybirdSettings`                     | missing                                                                              | missing                              |
| `[debug]`                  | `DebugConfig`                          | present on rc incl. `auction_html_comment_options`; `inject_adm_for_testing` missing | missing                              |

## Appendix C: Integration inventories (truth source for WP5 overview table)

Three inventories together describe the integration surface; no single
registry API exposes all of it, so WP8 tests them separately:

1. Registry registrations: `core/src/integrations/mod.rs` `builders()`
   (13 entries; capabilities below).
2. Auction providers: `core/src/auction/mod.rs` `provider_builders()`
   (adds `adserver_mock`; also registers prebid/APS providers).
3. JS modules: `registry.rs` module-id functions plus `JS_ALWAYS`
   (adds the always-injected `creative` module).

Capabilities: P proxy, AR attribute rewriter, SR script rewriter, HI head
injector, PP html post-processor, RF request filter, DJS deferred JS, AP
auction provider. Conditional capabilities are stated as such; registry
metadata alone does not expose them.

| ID                   | Capabilities                                                                    | JS module          | Docs page today                 |
| -------------------- | ------------------------------------------------------------------------------- | ------------------ | ------------------------------- |
| `prebid`             | P, AR, HI, DJS, AP                                                              | yes                | in sidebar                      |
| `aps`                | HI always, AP; P conditional on the trusted-server rendering mode; no JS bundle | render helper only | in sidebar                      |
| `datadome`           | P, AR, HI, RF (when protection on)                                              | yes                | in sidebar                      |
| `gpt`                | P, AR, HI                                                                       | yes                | orphaned                        |
| `gpt_diagnostics`    | standalone JS on demand                                                         | yes                | in sidebar                      |
| `google_tag_manager` | P, AR, SR                                                                       | yes                | orphaned                        |
| `didomi`             | P, HI                                                                           | yes                | in sidebar                      |
| `sourcepoint`        | P, AR, HI                                                                       | yes                | orphaned                        |
| `osano`              | bare registration                                                               | yes                | in sidebar                      |
| `permutive`          | P, AR                                                                           | yes                | in sidebar (thin)               |
| `lockr`              | P, AR                                                                           | yes                | in sidebar                      |
| `nextjs`             | SR x2, PP, no JS                                                                | no                 | in sidebar                      |
| `testlight`          | P, AR                                                                           | yes                | none                            |
| `adserver_mock`      | AP only (auction-provider inventory, no registry registration)                  | no                 | none                            |
| `creative` (JS-only) | always injected (`JS_ALWAYS`)                                                   | yes                | covered via creative-processing |

## Appendix D: CLI tree and environment variables

`ts` commands (from `crates/trusted-server-cli/src/run.rs` on rc/202608;
the canonical reference is the built binary's recursive `--help` tree,
which the WP8 golden file captures, including flags owned by the
lockfile-resolved `edgezero-cli`):
`audit page|generate|ad-templates generate|verify`, `active-version`,
`auth login|logout|status`, `build`,
`config init|diff|push|validate|gc|ad-templates lint|match|check|explain`,
`deploy`, `healthcheck`, `prebid bundle`, `provision`, `rollback`, `serve`,
`dev proxy [ca path|install|uninstall|regenerate]` (macOS only; `ts dev`
lists no subcommands on other hosts). `ts --version` is available
(`#[command(version)]`). Commands that detect drift (`config diff`,
`config ad-templates check`, audit verification) report a distinct drift
outcome with a stable exit code. `docs/guide/cli.md` must add
`active-version`, `healthcheck`, `rollback`, and `config gc`.

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

Compact index of audit findings driving WP1/WP2; verified against the main
baseline and re-verified on rc/202608 where marked.

- Dead endpoints documented: `docs/guide/api-reference.md:86,191,707,711`
  (rc; `/first-party/ad`, `/third-party/ad`);
  `docs/guide/integrations-overview.md:46-48`;
  `docs/guide/error-reference.md:658`;
  `docs/guide/integrations/prebid.md:515-531`.
- Dead operator commands: `docs/guide/error-reference.md:597`
  (`npm run type-check`), `:663` (`--validate-config`).
- Obsolete API examples: `docs/guide/key-rotation.md:301-310`
  (`KeyRotationManager::new(...)?`; constructor returns `Self`),
  unauthenticated admin curl examples;
  `docs/guide/configuration.md:1954-1957` (rc; nonexistent
  `settings_data::get_settings`, `println!` against repo conventions).
- Fabricated content: `docs/guide/ad-serving.md:11-18,43,48,77-83` (Equativ,
  `[ad_servers]`, `trackImpression`); `docs/guide/architecture.md:97-104`
  (`RequestWrapper`); `docs/guide/integration-guide.md:313` (equativ bidder).
- Integration-guide snippets that do not compile:
  `integration-guide.md:96` (`handle` without `RuntimeServices`, vs
  `registry.rs:282-288`), `:132` (`proxy_request` without `services`, vs
  `proxy.rs:737-742`), `:134` (`use fastly::http` in core-neutral code).
- Fictional-data policy violations: `docs/guide/ec-setup-guide.md:15` (real
  deployment domain); `.env.example:8-10` (non-reserved `publisher.com`).
- Wrong config names: `docs/guide/getting-started.md:141` (`[gdpr]`).
- Nonexistent builder method: `.with_asset(...)` in
  `docs/guide/creative-processing.md:808`,
  `docs/guide/integration-guide.md:84,248` (issue #277).
- Script-guard mechanism absent from the integration guide (issue #341).
- Old crate layout: `docs/roadmap.md:21-22` (the only surviving instance).
- Adapter support contradictions: `docs/index.md:27`,
  `docs/guide/what-is-trusted-server.md:32`, `docs/roadmap.md:19,34-38` vs
  `docs/guide/architecture.md:154-159`; Axum described as a deployment
  target; Spin described as production-capable despite `build_state()`
  loading the checked-in example toml and `startup_error_router()`
  serving a blanket 503 on startup failure (`adapter-spin/src/app.rs`);
  asset routes, request
  filters, image optimizer, and Tinybird emission are Fastly-only but
  documented as generic.
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
- Publishing: 120 `docs/superpowers/**` markdown files built into the public site (no
  `srcExclude`); `docs/guide/onboarding.md` published with internal
  contacts; `docs/public/CNAME` placeholder; empty `docs/guide/index.md`;
  nav Guide link bypasses the landing page (`config.mts:61`);
  `docs/package.json` not private, ISC license in an Apache-2.0 repo;
  Pages deploys only from `main`, so rc-merged containment does not reach
  the live site (`deploy-docs.yml:3`).
- Root-doc drift: `CLAUDE.md:102` (no workspace default target exists);
  `CONTRIBUTING.md` stale since 2026-01.
- Slash-command drift: `.claude/commands/{check-ci,verify,test-all}.md` omit
  Spin/cloudflare-wasm/parity gates; `test-crate.md` untargeted `cargo test`.
- Tooling: no `cargo doc` in CI; doctests never run (cross-compile only);
  `format-docs` never runs `vitepress build`; `deploy-docs.yml` not
  triggered by `.tool-versions` changes; CodeQL PR analysis limited to
  `main` branches; `eslint-plugin-jsdoc` inert;
  `openrtb-codegen` missing `[lints] workspace = true`; PR template says
  `tracing`; no semantic parity checks for routes, config, CLI,
  integrations, crates, navigation, or CI gates, and no binding between
  inventories and the reader-facing markdown.
