# Documentation Proposal Automation Design

- Status: proposed
- Design date: 2026-10-01
- Issue: #1105
- Target branch: `main`

## Purpose

When a merge to `main` changes behavior that the public documentation
describes, a reviewable pull request with the proposed documentation diff
should appear automatically. Today `deploy-docs.yml` only builds and deploys
the VitePress site after documentation changes land; nothing proposes edits
after a code merge.

This design adds a proposal path that never writes to `main`. It is a
deliberate, scoped addition to the documentation refresh design
(`2026-08-19-documentation-refresh-design.md`), which excluded automated
tracked-file write paths. The exclusion still holds for `main`: automation
only ever writes to a dedicated proposal branch and a pull request.

## Decisions

| Question                                     | Decision                                                                                                                                  |
| -------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| How is prose produced?                       | GitHub Copilot CLI, run headless in GitHub Actions with the workflow `GITHUB_TOKEN` (`copilot-requests: write`), billed to the org.       |
| Which code changes are examined?             | Pushes to `main` touching `crates/**`, `trusted-server.example.toml`, `edgezero.toml`, `fastly.toml`, `.cargo/config.toml`, `scripts/**`. |
| Which pages may change?                      | `docs/guide/**` and `docs/index.md` only — the public site validated by the docs gates.                                                   |
| How is prose checked against merged code?    | Copilot cites the merged source for every hunk; a per-hunk review exposes the evidence and a one-click revert; a human approves.          |
| How do sequential or overlapping merges map? | One pull request per merge commit on branch `docs/auto/<short-sha>`; runs are serialized.                                                 |
| What happens on an empty diff?               | No pull request. An existing proposal pull request for that SHA is closed.                                                                |
| What prevents duplicates on retry?           | The branch name is derived from the merge SHA; a retry force-updates the same branch and edits the same pull request.                     |

## Workflow shape

`.github/workflows/docs-proposal.yml` has two jobs. Copilot never holds a
token that can push or open pull requests.

### Trigger

- `push` to `main`, filtered to the source paths above. `docs/**` and
  `scripts/docs-proposal/**` are not included, so a merged documentation
  proposal or a change to this tooling never retriggers the workflow.
- `workflow_dispatch` with a required `sha` input, for retries and for the
  first end-to-end validation. Both scripts require a full 40-character SHA
  reachable from `origin/main`.
- `concurrency: { group: docs-proposal, cancel-in-progress: false }` so
  sequential merges are processed one at a time, in order.

### Job `propose`

Permissions: `contents: read`, `copilot-requests: write`.

1. Check out the merge commit with full history.
2. Install Copilot CLI (`@github/copilot`) at a pinned version.
3. `scripts/docs-proposal/propose.sh <sha> .docs-proposal`:
   1. Renders `.docs-proposal/prompt.md` from `scripts/docs-proposal/prompt.md`,
      the merge SHA, its subject, and the Sources-of-truth table from the
      refresh design.
   2. Runs `copilot -p` with `--no-ask-user`, `--allow-tool=write`, and
      `git show`, `git diff`, `git log`, and `git grep` shell tools; `git push`
      and `git commit` are denied, and file reads need no permission. Copilot
      edits documentation only when the merged change alters reader-facing
      behavior, and writes `.docs-proposal/rationale.md`.
   3. Runs `npm ci` and `npm run format:write` in `docs/` so Prettier
      normalizes the edits before any hunk is identified.
   4. Writes `proposal.patch` (`git diff --cached --binary <sha> -- docs`). An
      empty patch ends the job here.
   5. Runs `node scripts/docs-proposal/hunks.mjs list` to enumerate the hunks.
      A hunk id is the changed path plus a short hash of the hunk's removed
      and added lines, so it is stable across reruns. A second Copilot run
      writes `evidence.json`: for each hunk id, the merged source `path:line`
      that justifies it and a one-sentence reason.
4. Upload the `.docs-proposal/` directory as an artifact.

### Job `publish`

Permissions: `contents: write`, `pull-requests: write`. Copilot does not run.

`scripts/docs-proposal/publish.sh <sha> .docs-proposal`:

1. If a closed or merged pull request already exists for
   `docs/auto/<short-sha>`, leave it alone and exit successfully: a
   maintainer's decision is never reopened or recreated.
2. Empty diff: close any open pull request for that branch with a comment,
   then exit successfully. Otherwise apply `proposal.patch` to the merge
   commit.
3. Reject the proposal if any changed path is outside `docs/guide/**` or
   `docs/index.md`.
4. In `docs/`: `npm ci`, `npm run lint`, `npm run format`, `npm run build`.
   Any failure fails the job; no pull request is offered.
5. Commit as `github-actions[bot]` on `docs/auto/<short-sha>`. If the remote
   branch already exists and its tree equals the new tree, stop: the proposal
   is unchanged and nothing is re-posted. Otherwise force-push.
6. Find the originating pull request with
   `gh api repos/{owner}/{repo}/commits/<sha>/pulls`. Create the pull request,
   or edit the existing one, with a body that links the merge commit and the
   originating pull request, includes `rationale.md`, and states that a human
   must verify the prose against the code before merging.
7. `node scripts/docs-proposal/hunks.mjs review` builds review comments and
   posts one `COMMENT` review through `gh api`.

## Per-hunk review

Each hunk in the pull request receives one review comment containing the
Copilot evidence for that hunk and a `suggestion` block that **reverts** the
hunk. Reviewers accept the prose by leaving it, and reject a hunk by applying
its suggestion. Suggestions are built from the diff, never from model output,
so the revert is exact:

| Hunk kind                 | Comment anchor                          | Suggestion body                          |
| ------------------------- | --------------------------------------- | ---------------------------------------- |
| Changed or inserted lines | The new lines (right side)              | The original lines (empty for insertion) |
| Pure deletion             | Adjacent context line (prefer previous) | That context line plus deleted lines     |
| Deleted file or binary    | Listed in the review body               | None                                     |

Hunks without evidence still receive a revert suggestion, marked "none
cited". Copilot output that names unknown hunk ids is ignored, and evidence
text is stripped of backticks, newlines, and `@` mentions before it is posted.

GitHub only allows suggestions on lines inside the pull request diff, which is
why the edits are commits and suggestions are reverts.

## Repository prerequisites

Documented in `scripts/README.md`:

- The organization Copilot policy "Allow use of Copilot CLI billed to the
  organization" is enabled.
- Repository setting "Allow GitHub Actions to create and approve pull
  requests" is enabled.
- Pull requests opened with `GITHUB_TOKEN` do not trigger other workflows.
  The `publish` job runs the docs gates itself; maintainers re-run regular CI
  by pushing to the branch if needed.

## Security

- Copilot runs with a read-only `contents` token and an allowlist of read-only
  shell tools; it has no network shell, no push, and no pull request
  permissions.
- Input is code already merged to `main`, so no untrusted fork content reaches
  the agent.
- The path allowlist is enforced by `publish.sh`, not by the prompt.
- Proposed prose must use fictional `example.com` values, per `CLAUDE.md`;
  the reviewer verifies this before merging.

## Testing

- `node --test scripts/docs-proposal/hunks.test.mjs` covers hunk listing,
  every row of the per-hunk table, unknown evidence ids, and the review payload.
- `scripts/docs-proposal/test.sh` stubs `gh` and `npm` and pushes to a
  temporary bare repository to cover the empty diff, closing a stale
  proposal, a disallowed path, first publish, an unchanged retry, an updated
  proposal, a closed proposal, and PR body rendering.
- `shellcheck` passes for every script.
- The `docs-proposal-scripts` job in `format.yml` runs all three on every
  pull request.
- Copilot cannot run locally; the first `workflow_dispatch` run against a
  recent merge SHA is the end-to-end test.

## Out of scope

- Crate READMEs, `docs/internal/**`, and `docs/superpowers/**`.
- Rolling or merged proposals across several merges.
- A GitHub App token to trigger regular CI on proposal pull requests.
