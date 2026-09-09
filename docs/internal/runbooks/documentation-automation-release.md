# Documentation Automation Release Verification

Use this runbook only after PR #1049 has reached `main`. These checks produce
release evidence; they are not implementation gates and must not be replaced by
local builds, fixtures, mocked responses, or runs from an rc branch.

The release owner is `aram356`. Start within one business day of the merge and
finish within two business days. If any required check fails, open one focused
repair issue within one business day, assign it to `aram356`, and link both the
failure receipt and repair issue from PR #1049.

## Evidence handling

Append each receipt as a new comment on PR #1049 using the capture template in
`docs/internal/audits/documentation-refresh-evidence.md`. The PR comment URL is
the canonical capture destination. Never edit or delete a receipt. A correction
must be a new comment that names the superseded capture ID and comment URL.

Before posting, remove tokens, credential-bearing headers, cookies, and secrets.
Include the actual redacted request body, response body, and applicable API JSON;
record each body's UTF-8 byte length and SHA-256. Keep each comment at or below
60 KiB. Split larger captures into ordered chunks and record every chunk's index,
byte length, and SHA-256 plus the aggregate byte length and SHA-256. URLs are
navigation aids; the pasted redacted bodies and hashes are the evidence.

## Pages and CNAME

1. Resolve the merge commit with `git rev-parse origin/main` and verify the
   successful Deploy VitePress Docs run used that exact SHA.
2. Record the run ID, attempt, build and deploy job URLs, GitHub App identity,
   deployment URL, response status, redirect chain, canonical URL, and relevant
   response headers.
3. Request the site root and every required route listed by the docs-parity
   route manifest. Require expected page content and HTTP success.
4. Request every excluded or retired route. Require the exact absence behavior
   specified by the route manifest; a soft-404 page does not count as absent.
5. Parse the deployed HTML and request representative CSS, JavaScript, image,
   and font assets. Require every asset URL to retain the repository project
   path and return successfully.
6. Require `docs/public/CNAME` to be absent at the deployed source SHA. Record
   DNS, redirect, TLS hostname, and canonical-link observations proving that no
   placeholder or custom-domain CNAME remains.

State remains `release-pending` until this live matrix is captured from the
deployed `main` SHA.

## First scheduled external-link run

1. Wait for the first `17 9 * * 1` scheduled Documentation automation run on
   `refs/heads/main`; a manual dispatch is not the first-schedule receipt.
2. Require the `link-reader` and `issue-writer` jobs to use the same run ID and
   attempt, to finish inside their 30- and 5-minute limits, and to show that the
   non-canceling `documentation-automation-default-branch-writers` concurrency
   contract was honored.
3. Record the run, job, and artifact URLs; source SHA and ref; GitHub App
   identities; archive byte length and SHA-256; inner `link-results.json` byte
   length and SHA-256; and the validator result.
4. Record the single owned issue's URL and final state. Require creation or
   update when findings exist, automatic closure when the complete result is
   clean, no duplicate owned issue, and no modification of a foreign issue.

State remains `release-pending` until the real scheduled run and reconciliation
are captured.

## First dependency submission

1. From the same first scheduled run, require `dependency-reader` and
   `dependency-writer` to use one run ID and attempt and to finish inside their
   20- and 5-minute limits. Require the writer to check out only the
   authenticated `github.sha` with credentials disabled and to execute only
   `scripts/dependency-snapshot-submit.sh` from that checkout. The script must
   verify the same-run artifact digest and immutable GitHub context, delegate
   archive and schema validation to docs-parity, and submit only the resulting
   canonical JSON.
2. Record the authenticated `main` ref and SHA, archive and inner JSON byte
   lengths and SHA-256 values, fixed detector `trusted-server-docs-parity`, fixed
   correlator `trusted-server-docs-parity-v1`, and snapshot ID.
3. Capture the redacted POST request to
   `repos/IABTechLab/trusted-server/dependency-graph/snapshots` and its exact
   HTTP 201 response.
4. Query the dependency graph for the submitted SHA. Capture the redacted JSON
   response and require the submitted manifests and packages to be visible.
5. Assign any rejected, missing, or stale dependency to `aram356`; triage within
   one business day and resolve or open a repair PR within two business days.

State remains `release-pending` until both the 201 receipt and graph visibility
are captured.

## Dependabot and action release versions

At the merged `main` SHA, inspect `.github/dependabot.yml`. Require exactly the
GitHub Actions root plus the root Cargo workspace, docs-parity Cargo workspace,
JavaScript library, browser tests, Next.js fixture, and docs npm roots; require
weekly cadence and `target-branch: "main"` for all seven entries.

Inspect every `uses:` entry in tracked workflow and composite-action YAML.
Require normalized local paths or exact external release-version tags. Resolve
each external version through its publisher's release or tag and record the
observed version and primary release URL. Confirm every newly added
multi-command workflow step delegates to a reviewed repository script and that
the documentation writers contain no Python. Record `.tool-versions` and
require Wrangler `4.129.0`.

## Optional `main` protection

No branch-protection change is selected by this documentation refresh. This
check remains optional and has no owner or capture URL unless maintainers make a
separate recorded decision.

If maintainers later opt in, wait until every proposed required context has
reported on `main` from the expected GitHub App. Then record exact context names,
App identities, strictness, bypass policy, redacted ruleset/protection/branch API
bodies with byte lengths and SHA-256 values, and one planted-failure proof that
blocks merging. Do not seed statuses, accept similarly named contexts, or change
protection before that evidence exists.
