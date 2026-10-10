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
| What prevents duplicates on retry?           | The branch name is derived from the merge SHA; a retry updates that branch and pull request, but never over maintainer commits.           |

## Workflow shape

`.github/workflows/docs-proposal.yml` has three jobs. Copilot never holds a
token that can push or open pull requests, and proposal content is never built
or executed in a job that can.

Every job checks out the workflow's own revision into `tools/` and the merge
commit into `source/`, then runs the scripts from `tools/` against `source/`.
Retrying a merge that predates this tooling therefore works, and a retry runs
the current scripts rather than historical ones.

### Trigger

- `push` to `main`, filtered to the source paths above. `docs/**` and
  `scripts/docs-proposal/**` are not included, so a merged documentation
  proposal or a change to this tooling never retriggers the workflow.
- `workflow_dispatch` with a required `sha` input and an optional `base`
  input, for retries and for the first end-to-end validation. All three
  scripts require a full 40-character SHA reachable from `origin/main`.
- `concurrency` with `group: docs-proposal`, `cancel-in-progress: false`, and
  `queue: max` so sequential merges are processed one at a time, in order.
  The default queue keeps one pending run and cancels the rest, which would
  skip merges.

### Job `propose`

Permissions: `contents: read`, `copilot-requests: write`.

1. Check out the tools and the merge commit with full history.
2. Install Copilot CLI (`@github/copilot`) at a pinned version.
3. `scripts/docs-proposal/propose.sh <sha> .docs-proposal <base>`:
   1. Renders `.docs-proposal/prompt.md` from `scripts/docs-proposal/prompt.md`,
      the merge SHA, its base, its subject, and the Sources-of-truth table from
      the refresh design. The base is the push's previous `main` head, or the
      manual dispatch's optional `base` input, falling back to the first
      parent when it is missing or not an ancestor, so Copilot inspects
      `git diff <base> <sha>`: a multi-commit push is covered in full, and a
      clean merge commit is not hidden by `git show`'s empty combined diff.
   2. Runs `copilot -p` with `--no-ask-user`, `--allow-tool=write`, and
      `git show`, `git diff`, and `git log` shell tools; `git push` and
      `git commit` are denied, and file reads need no permission. `git grep`
      is not allowed because `--open-files-in-pager` runs a command. Copilot
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
      that justifies it and a one-sentence reason. Evidence is advisory, so a
      failed evidence run only logs a warning and keeps the proposal.
4. Upload the `.docs-proposal/` directory as an artifact.

### Job `validate`

Permissions: `contents: read`; checkouts do not persist credentials.
VitePress executes Vue in Markdown during the build, so generated content runs
only here.

`scripts/docs-proposal/validate.sh <sha> .docs-proposal`:

1. Empty diff: exit successfully.
2. Apply `proposal.patch` to the merge commit and reject the proposal if any
   changed path is outside `docs/guide/**` or `docs/index.md`, or any changed
   entry is not a regular file. A symlink would otherwise pass the path check
   and publish its target.
3. In `docs/`: `npm ci`, `npm run lint`, `npm run format`, `npm run build`.
   Any failure fails the workflow; no pull request is offered.

### Job `publish`

Permissions: `contents: write`, `pull-requests: write`. Copilot does not run,
and nothing installs, builds, or executes proposal content.

`scripts/docs-proposal/publish.sh <sha> .docs-proposal`:

1. If a closed or merged pull request already exists for
   `docs/auto/<short-sha>`, leave it alone and exit successfully: a
   maintainer's decision is never reopened or recreated. Pull requests from
   forks that reuse the branch name are ignored.
2. If an open pull request records a different base in its body, fail: the
   retry inspected another range, such as a manual dispatch without the base
   of a multi-commit push, and must not narrow or close the proposal. The
   error names the recorded base to rerun with.
3. Look up the remote branch with `git ls-remote --exit-code`. Only a
   verified absent branch counts as new; any other lookup or fetch failure
   fails the run, so an error never hides maintainer commits. If the remote
   branch's head is not the generated proposal commit (its
   parent is not the merge commit), a maintainer has committed there, for
   example by applying a review suggestion. Warn and exit successfully
   without pushing, closing, or deleting anything.
4. Empty diff: if a pull request is open, delete its branch with
   `git push --force-with-lease=refs/heads/<branch>:<inspected head> --delete`,
   so a maintainer push after the inspection makes the deletion fail. Only
   after the deletion succeeds, comment on the pull request and then close
   it; the deletion may already have closed it, and `gh pr close` skips its
   `--comment` on a closed pull request.
   Then exit successfully. Otherwise apply `proposal.patch` to a fresh
   checkout of the merge commit.
5. Reject the proposal again if any changed path is outside `docs/guide/**` or
   `docs/index.md`, or any changed entry is not a regular file.
6. Commit as `github-actions[bot]` on `docs/auto/<short-sha>`. If the remote
   branch already exists and its tree equals the new tree, reuse the remote
   commit and skip the push, so a retry resumes any later step that failed.
   Otherwise push with a lease on the fetched head.
7. Find the originating pull request with
   `gh api repos/{owner}/{repo}/commits/<sha>/pulls`. Create the pull request,
   or edit the existing one, with a body that links the merge commit and the
   originating pull request, records the inspected base, includes `rationale.md` as a
   fenced `text` block with its `@` mentions neutralized, so links, HTML,
   issue references, and closing keywords in it stay inert, and states that
   a human must verify the prose against the code before merging.
8. If `github-actions[bot]` already reviewed the pushed commit, stop: nothing
   is re-posted. Otherwise `node scripts/docs-proposal/hunks.mjs review`
   builds review comments and posts one `COMMENT` review through `gh api`
   against the pushed commit.

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
| New file                  | The new lines (right side)              | None; the comment asks to delete it      |
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
  The `validate` job runs the docs gates itself. The checks `main` requires
  never start on a proposal, so maintainers close and reopen it, or push to
  its branch, before merging.

## Security

- Copilot runs with a read-only `contents` token and an allowlist of git
  shell tools; it has no network shell, no push, and no pull request
  permissions.
- The tool allowlist is not the boundary. Copilot can write any file in the
  checkout, and `propose.sh` then runs `npm` in `docs/`, so the `propose` job
  is treated as able to run arbitrary code. Its token is read-only, neither it
  nor `validate` restores or saves a dependency cache, and its artifact is
  re-checked by `validate.sh` and `publish.sh`.
- Both jobs run on the `main` ref, and code running in a job can save cache
  entries scoped to `main` without declaring a cache. `deploy-docs.yml`, the
  only privileged workflow that restored a cache, therefore installs docs
  dependencies without one.
- Input is code already merged to `main`, so no untrusted fork content reaches
  the agent.
- The path allowlist is enforced by `validate.sh` and again by `publish.sh`,
  not by the prompt.
- Generated Markdown is built only in the `validate` job, which has a
  read-only token and no persisted credentials.
- Proposed prose must use fictional `example.com` values, per `CLAUDE.md`;
  the reviewer verifies this before merging.

## Testing

- `node --test scripts/docs-proposal/hunks.test.mjs` covers hunk listing,
  every row of the per-hunk table, unknown evidence ids, and the review payload.
- `scripts/docs-proposal/test.sh` stubs `gh` and `npm` and pushes to a
  temporary bare repository to cover validation of empty, disallowed, symlinked,
  and allowed proposals, the empty diff, fork pull requests that reuse the
  proposal branch name, closing a stale proposal, a disallowed
  path, first publish without a build, an unchanged retry, an updated proposal
  whose review fails and is resumed without a second push, a retry or empty
  rerun that leaves maintainer commits in place, an empty rerun that races a
  maintainer push, a failed branch fetch on both the empty and push paths,
  deleting an unmaintained branch before closing its proposal, retries over a different or
  unrecorded range, a closed proposal, PR body rendering, and how
  `propose.sh` resolves the base of a multi-commit push, renders the work
  dir into prompts, and runs, or survives a failure of, the evidence half.
- `shellcheck` passes for every script.
- The `docs-proposal-scripts` job in `format.yml` runs all three on every
  pull request.
- Copilot cannot run locally; the first `workflow_dispatch` run against a
  recent merge SHA is the end-to-end test.

## Out of scope

- Crate READMEs, `docs/internal/**`, and `docs/superpowers/**`.
- Rolling or merged proposals across several merges.
- A GitHub App token to trigger regular CI on proposal pull requests.
