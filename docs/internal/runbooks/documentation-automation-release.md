# Documentation Release Verification

Use this runbook after PR #1049 reaches `main`. It verifies the deployed
VitePress site. Repository tests and pull-request checks do not prove DNS, TLS,
redirect, or hosted-content behavior.

## Identify the deployment

1. Resolve the merge commit with `git rev-parse origin/main`.
2. Find the successful Deploy VitePress Docs workflow run for that exact SHA.
3. Record the run ID, attempt, build job, deploy job, deployment URL, and GitHub
   App identity.
4. Stop if the deployed source SHA differs from the merge commit under review.

## Verify the project-path site

Request the deployed project root and representative pages from every navigation
group:

- landing and getting started;
- configuration and EdgeZero;
- API and all four adapter guides;
- integrations and auction testing;
- telemetry, JavaScript, CLI, errors, and testing.

Require HTTP success, expected page-specific content, and a canonical URL under
the `/trusted-server/` project path.

Parse deployed HTML and request representative CSS, JavaScript, image, and font
assets. Every local asset URL must preserve the project path and return
successfully.

## Verify containment

Require no public route or navigation entry for:

- `docs/internal/**`;
- `docs/superpowers/**`;
- repository-team onboarding;
- the archived FAQ proof of concept; or
- the unverified business-use-case source.

A themed soft-404 is not proof of absence. Record the response status, final
URL, and enough response text to distinguish an absent route from published
content.

## Verify CNAME behavior

At the deployed source SHA, require `docs/public/CNAME` to be absent. Record:

- DNS answers for the GitHub Pages host;
- redirect chain from the project URL;
- TLS hostname and certificate result;
- canonical link; and
- final asset paths.

Do not infer custom-domain state from repository content alone.

## Evidence handling

Post the deployment receipt to PR #1049 or the release record selected by the
maintainers. Include the exact deployed SHA and timestamp. Remove tokens,
cookies, credential-bearing headers, and secrets before posting.

If verification fails, open one focused repair issue containing the failed URL,
expected behavior, observed status/redirect/content, deployed SHA, and workflow
run. Do not weaken containment or restore the placeholder `CNAME` as a repair.
