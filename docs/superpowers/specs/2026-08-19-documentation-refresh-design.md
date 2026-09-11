# Documentation Refresh Design

- Status: approved and implemented on PR #1049
- Design date: 2026-08-19
- Last revised: 2026-09-11
- Target branch: `rc/202608`
- Audited base: `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf`

## Purpose

Trusted Server documentation had grown around the original Fastly-only
implementation. It did not describe the current EdgeZero configuration
lifecycle, four adapters, auction provider model, JavaScript bundle model,
operator CLI, or the boundaries between public and internal material. Some
pages also mixed future intent with shipped behavior.

This refresh makes the maintained documentation describe the code that ships.
It is a documentation and verification change, not a new repository tooling
platform. It stays in PR #1049; no auxiliary implementation PR is required.

## Scope guardrails

The refresh includes:

- public navigation, getting-started, configuration, API, adapter, integration,
  auction, telemetry, JavaScript, CLI, and testing material;
- crate READMEs and missing Rust or TypeScript API documentation;
- real adapter smoke scripts and existing CI gates needed to validate the
  documented commands;
- publishing containment for internal and archived material; and
- concise design, plan, decision, and evidence records.

The refresh does not include:

- a new documentation-analysis binary or separate Cargo workspace;
- checked generator manifests, CLI help goldens, or source-classification
  inventories;
- scheduled external-link issue writers or dependency-submission automation;
- branch-protection changes, deployment credentials, or a second PR; or
- product refactors whose only purpose is to support documentation tooling.

These exclusions are architectural boundaries. Normal product regression tests
and the existing documentation-site, rustdoc, doctest, and snippet gates remain.

## Sources of truth

Documentation is maintained against the following implementation surfaces.

| Contract                                              | Authoritative source                                         | Reader-facing destination                             |
| ----------------------------------------------------- | ------------------------------------------------------------ | ----------------------------------------------------- |
| Application fields, defaults, validation, and secrets | `trusted-server.example.toml`, `settings.rs`, `config.rs`    | Configuration and EdgeZero guides                     |
| Public routes and degraded startup behavior           | Adapter router construction and route tests                  | API reference and adapter guides                      |
| Adapter maturity and limitations                      | Adapter implementations, manifests, and tests                | Adapter support tables                                |
| Integration enablement and routes                     | Integration builders, auction plan registration, JS registry | Integration overview and per-integration guides       |
| Auction providers and profiles                        | `auction` modules and provider profile schemas               | Ad-serving, configuration, and auction-testing guides |
| CLI commands and flags                                | Native `ts --help` output and command implementations        | CLI guide                                             |
| Browser bundles and loading modes                     | `trusted-server-js` registry and build pipeline              | Trusted Server JavaScript guide                       |
| Verification commands                                 | Cargo aliases and GitHub workflows                           | `CLAUDE.md#ci-gates`                                  |
| Publishing behavior                                   | VitePress configuration and deploy workflow                  | Docs README and release runbook                       |

Tables in the guides are maintained summaries. They do not claim to be
generated or independently canonical. A change to an authoritative source must
update its reader-facing documentation in the same pull request.

## Audiences and information architecture

### Publisher and operator path

The public site must answer, in order:

1. what Trusted Server does and which runtimes are supported;
2. how to install prerequisites and create a local configuration;
3. how configuration is validated, pushed, and resolved at runtime;
4. how to deploy and verify the selected adapter;
5. how integrations, auction providers, browser bundles, and proxy routes work;
6. how to test and diagnose the deployment.

The public landing and getting-started material retain that journey. Onboarding
was not deleted: repository-team onboarding moved to
`docs/internal/onboarding.md` because it contains maintainer workflow details
rather than product installation instructions.

### Maintainer path

`README.md`, `CONTRIBUTING.md`, `CLAUDE.md`, `TESTING.md`, crate READMEs, and
internal records cover repository structure, coding constraints, test targets,
release evidence, and ownership boundaries. The CI matrix exists once in
`CLAUDE.md`; other files link to it and may add focused runbooks without copying
the matrix.

### Archived and excluded material

- `FAQ_POC.md` is historical and lives under `docs/superpowers/archive/`.
- `docs/business-use-cases.md` remains source-visible with an explicit
  unverified banner but is excluded from the public site.
- `docs/internal/**` and `docs/superpowers/**` are never public navigation or
  VitePress source inputs.
- Retired integration pages may remain as tombstones only when they clearly say
  the integration is unavailable.

## Public documentation deliverables

### Landing, architecture, and setup

The landing page states the supported runtimes and links directly to setup,
configuration, adapters, integrations, API, and testing. Architecture explains
the portable core, adapter boundary, EdgeZero lifecycle, browser bundle
pipeline, and auction orchestration without presenting obsolete Fastly-only
flows.

The getting-started path uses fictional domains and secrets, distinguishes
local drafts from deployable configuration, and sends operators through
validation before push or deployment.

### Configuration and EdgeZero

The configuration reference covers every top-level configuration family,
including publisher behavior, handlers, integrations, auction providers,
creative opportunities, request signing, trusted client IP, telemetry, debug,
and platform stores. It distinguishes:

- source TOML from the serialized app-config blob;
- secret key references from secret values;
- parse-time, deploy-time, startup-time, and request-time validation;
- logical EdgeZero store IDs from adapter-specific bindings; and
- provider wrapper fields from profile-specific `profile_config` fields.

The EdgeZero guide owns configuration initialization, validation, push,
envelope, store mapping, and restart/deploy semantics.

### API and adapters

The API reference records routes, methods, predicates, authentication, request
shape, success behavior, failure behavior, cache/CORS behavior, and adapter
availability. It separates normal routes from degraded startup routers.

Each adapter guide states maturity, health behavior, startup status, provider
fan-out, client-IP treatment, prerequisites, local handoff, deployment steps,
store/secret bindings, limitations, and a concrete verification path:

- Fastly: production, Viceroy-backed local smoke, multiple providers.
- Axum: development server, native tests, no request-time KV implementation.
- Cloudflare: development, one enabled provider, no normal health route.
- Spin: experimental, one enabled provider, startup `503` with liveness health.

### Integrations, auction, and JavaScript

The integration overview lists all deploy-accepted integration IDs and
separates registration from operational maturity. Per-integration pages explain
activation, configuration, routes or hooks, browser loading, and limitations.

Auction documentation defines provider instances, bidder routing, profiles,
mediation, timeouts, and testing. The JavaScript guide distinguishes integration
modules, emitted bundles, immediate/deferred/standalone loading, cache keys, and
the publisher-visible namespace.

### CLI and testing

The CLI guide covers the Linux/macOS command union, platform-only commands,
config, deploy, audit, development proxy, template, and Prebid bundle workflows.
Examples must be executable descriptions of current flags; platform
differences are explicit.

The public testing guide explains target-specific Cargo aliases, Viceroy,
adapter tests, browser fixtures, documentation snippets, and smoke scripts. It
links to the single complete gate matrix instead of duplicating it.

### READMEs and API documentation

Every workspace crate has a short README stating its role, target, entry points,
and focused verification command. Public Rust items and exported TypeScript
surfaces receive useful documentation where warnings or reader navigation
exposed gaps. Comments must explain behavior or constraints, not restate names.

## Adapter smoke contracts

All smoke scripts use isolated temporary state, start a real local runtime,
exercise missing configuration and required-secret failures, and finish with a
publisher request that proves origin proxying and URL rewriting. A health-only
response is never sufficient.

- Axum uses temporary config and store state.
- Cloudflare uses temporary Wrangler manifests and local KV state.
- Spin uses a temporary Spin manifest/KV store and reads the component stderr
  captured by Spin.
- Fastly copies `edgezero.toml` and `fastly.toml` into a per-run project. It
  never edits or restores the tracked manifest, so overlapping runs cannot
  overwrite one another or a user's edits.

Spin's startup diagnostic is written directly to component stderr. It does not
install a global logger, change the maximum log level, or suppress ordinary
application logs.

## Verification and CI

Existing mechanisms provide the enforcement boundary:

- VitePress ESLint, Prettier, and production build, including internal-link
  validation;
- Rust formatting and target-matched Clippy for all adapters and host tools;
- adapter, core, CLI, OpenRTB codegen, integration-parity, and
  documentation-snippet tests;
- Fastly and Spin release builds;
- rustdoc with warnings denied plus core doctests;
- JavaScript lint, formatting, tests, and production build; and
- real local adapter smoke scripts when the corresponding CLI is installed.

`scripts/check-documentation.sh` composes the site, JSDoc, snippet, doctest,
and rustdoc checks for local use and the manually dispatched Documentation
checks workflow. It owns no parser, manifest, generated output, or tracked-file
write path, and it cannot block normal core or adapter CI.

GitHub actions use exact release tags. A workflow step with more than one line
of executable logic delegates to a repository script. Pull-request workflows
remain read-only.

## Security and privacy

Examples use `example.com` or explicitly fictional values. Documentation must
not include live credentials, private contacts, customer identifiers, or
internal access instructions. Secrets are described as store keys and
provisioning actions, never embedded deployable values.

The checked-in Fastly service ID is a temporary, owner-approved compatibility
exception recorded in the decisions file. It expires on 2026-09-30 and must be
removed or explicitly renewed through review.

The VitePress site uses the repository project path and has no placeholder
`CNAME`. Internal and archived directories are excluded at the source boundary,
not merely hidden from navigation.

## Compatibility

The refresh does not change public application configuration shapes or normal
route behavior. Added adapter route tests document existing parity. Startup
fallback work is limited to preserving a real Spin diagnostic without claiming
global logging. CI additions exercise existing supported targets.

Removing the rejected tool proposal also removes its tool-manifest
`include_str!` tests; production modules no longer depend, even in test builds,
on a separate documentation proposal.

## Risks and mitigations

| Risk                                   | Mitigation                                                       |
| -------------------------------------- | ---------------------------------------------------------------- |
| Repeated tables drift                  | Name the implementation source and require same-PR updates       |
| Gate instructions drift                | Keep the full matrix only in `CLAUDE.md`                         |
| Public/internal leakage                | Exclude internal/archive sources in VitePress and test the build |
| Adapter claims exceed reality          | Tie each claim to router tests and a runtime smoke               |
| Smoke scripts alter user files         | Use per-run manifests and owned process cleanup                  |
| Logging workaround suppresses records  | Emit only the startup diagnostic directly to Spin stderr         |
| Examples expose sensitive data         | Use fictional values and review explicit exceptions              |
| Cross-target commands fail on the host | Use repository Cargo aliases and pinned targets                  |

## Acceptance criteria

The refresh is complete when:

- the public site builds with no dead internal links;
- getting-started and internal onboarding both have clear, separate homes;
- configuration, API, adapters, integrations, auction, JavaScript, CLI,
  telemetry, and testing are covered;
- every crate has an accurate README and API documentation gates pass;
- the complete test matrix appears only in `CLAUDE.md`;
- no rejected tool directory, tool workflow, tool script, generated marker, or
  product-test manifest receipt remains;
- Spin does not set a global logger;
- Fastly smoke does not write the repository manifests;
- action references use exact release tags and multiline workflow logic lives
  in scripts;
- target-matched tests, docs checks, JavaScript checks, rustdoc, and workflow
  structure checks pass; and
- all commits are pushed to PR #1049.
