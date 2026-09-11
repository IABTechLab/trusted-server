# Documentation Refresh Decisions

- Decision date: 2026-08-31
- Last revised: 2026-09-11
- Approver: `aram356`
- Delivery: PR #1049 to `rc/202608`
- Audited base: `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf`

This record contains decisions that cannot be inferred from product source. It
does not treat a moving branch head as immutable evidence.

## Delivery boundary

All repository changes ship through
https://github.com/IABTechLab/trusted-server/pull/1049. No individual,
containment, tooling, activation, or release-handoff PR is created.

PR #1104 is closed and superseded. Its reviewed containment changes were moved
to PR #1049; it has no merge or deployment claim.

## Documentation tooling scope

The custom analysis workspace, generator manifests, scheduled link writer, and
dependency-submission writer are not part of this refresh. They require a
separate architecture decision before they may enter the repository. PR #1049
retains the documentation, existing CI mechanisms, documentation snippets, and
adapter regression/smoke tests without coupling product tests to documentation
records.

## Public and internal content

### Repository-team onboarding

Move maintainer onboarding from `docs/guide/onboarding.md` to
`docs/internal/onboarding.md` and exclude it from the public build. Preserve the
public installation and setup journey in the landing, getting-started,
configuration, and adapter guides.

### CNAME

Delete `docs/public/CNAME` and retain VitePress's `/trusted-server` project base.
Do not restore the old placeholder. A future custom domain requires separately
verified Pages, DNS, TLS, redirects, canonical URLs, and asset paths.

### FAQ proof of concept

Archive `FAQ_POC.md` under `docs/superpowers/archive/`. Do not publish it as an
active FAQ. Existing integration tombstones remain independently maintained.

### Business use cases

Keep `docs/business-use-cases.md` source-visible with an unverified-content
banner and exclude it from the public site. A verified rewrite can republish it
later.

## Governance

Describe only current governance evidence. Do not invent meeting minutes,
release cadence, CODEOWNERS, or named maintainers. Governance expansion is a
maintainer decision outside this refresh.

## Fastly service ID exception

- Type: service ID
- Scope: the existing `service_id` in root `fastly.toml`
- Owner: `aram356`
- Rationale: preserve the active service binding during the documentation
  refresh
- Expiry: `2026-09-30T00:00:00Z`

This is not a blanket sensitive-data exception. The owner must remove the value
or approve a new explicit expiry before that date.

## Examples and sensitive material

Use fictional credentials, `example.com` domains, and non-customer identifiers.
Do not include private contacts, internal access instructions, live tokens,
cookies, or deployable secrets. Public vendor URLs are allowed when necessary
to document an integration.

## CI gate ownership

`CLAUDE.md#ci-gates` is the single complete command matrix. `AGENTS.md` points
to `CLAUDE.md`; `TESTING.md` and the public testing guide add focused navigation
without copying the matrix.

Pull-request workflows remain read-only. Actions use exact release tags. New
workflow steps containing more than one line of executable logic must call a
repository script.

The aggregate documentation checker is available through a manual-only GitHub
Actions workflow. It is not a required core or adapter status check.

## Adapter smoke and logging decisions

Every adapter smoke uses isolated temporary state and a real local runtime.

Fastly smoke operates on copied manifests in a per-run project. It does not
lock, modify, or restore the root manifest because it never writes it.

Spin emits the one startup-failure diagnostic directly to component stderr.
The adapter does not install a global logger, lower the maximum log level, or
claim ownership of application logging.

## Evidence policy

Historical receipts name immutable commit SHAs and remain true only for those
commits. Final acceptance is the exact pushed SHA and hosted checks visible on
PR #1049. Internal records do not embed a self-referential “current exact head”
claim.

Live Pages behavior can only be confirmed after deployment from `main`. Local
VitePress output proves build structure, not DNS, TLS, redirects, or hosted
content.
