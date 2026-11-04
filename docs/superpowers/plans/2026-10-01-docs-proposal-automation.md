# Documentation Proposal Automation Implementation Plan

> **Status: Superseded.** This is the original two-job plan, kept for history.
> Review moved the docs gates into the read-only `validate` job, replaced
> `--force` pushes and `gh pr close --delete-branch` with leased pushes, dropped
> `shell(git grep:*)` and the npm cache, and added base tracking. Do not execute
> it; the design spec and `scripts/docs-proposal/` are the source of truth.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** After a code merge to `main`, run GitHub Copilot CLI to propose
documentation edits, and open one pull request per merge whose hunks each carry
a review comment citing the merged source and a suggestion that reverts the
hunk.

**Architecture:** A two-job workflow. `propose` (read-only token plus
`copilot-requests: write`) runs Copilot and uploads a patch, rationale, and
per-hunk evidence. `publish` (write token, no Copilot) applies the patch,
enforces a path allowlist, runs the docs gates, pushes `docs/auto/<sha12>`,
opens or edits the pull request, and posts a review built deterministically
from the diff by a dependency-free Node script.

**Tech Stack:** GitHub Actions, Copilot CLI (`@github/copilot@1.0.90`), Bash,
Node 24 (`node:test`, no npm dependencies), `gh` CLI, VitePress docs gates.

**Spec:** `docs/superpowers/specs/2026-10-01-docs-proposal-automation-design.md`

## Global Constraints

- Copilot may only change `docs/guide/**` and `docs/index.md`; `publish.sh`
  enforces it with `^docs/(guide/.+|index\.md)$`.
- Branch name: `docs/auto/<first 12 hex chars of merge SHA>`.
- One pull request per merge SHA; a closed or merged proposal is never
  reopened or recreated.
- Empty diff: no pull request; an open proposal for that SHA is closed.
- Docs gates before any push: `npm ci`, `npm run lint`, `npm run format`,
  `npm run build` in `docs/`.
- Copilot never holds a token with `contents: write` or `pull-requests: write`;
  checkout in `propose` uses `persist-credentials: false`.
- Action references match the repository's existing tags (`actions/checkout@v4`,
  `actions/setup-node@v4`, `actions/upload-artifact@v4`,
  `actions/download-artifact@v4`). Workflow steps are single-line; logic lives
  in `scripts/docs-proposal/`.
- Merge SHAs are full 40-character lowercase hex and must be reachable from
  `origin/main`.
- Comments above code, never inline. Fictional `example.com` values only.
- Commit messages: sentence case, imperative, no prefixes; `git commit -S --signoff`.

## File Map

| File                                               | Responsibility                                               |
| -------------------------------------------------- | ------------------------------------------------------------ |
| `scripts/docs-proposal/hunks.mjs`                  | Parse a unified diff, list hunks with ids, build the review  |
| `scripts/docs-proposal/hunks.test.mjs`             | `node:test` coverage for `hunks.mjs`                         |
| `scripts/docs-proposal/lib.sh`                     | Shared Bash helpers: SHA validation, branch, allowlist, body |
| `scripts/docs-proposal/prompt.md`                  | Copilot instructions for proposing edits                     |
| `scripts/docs-proposal/evidence-prompt.md`         | Copilot instructions for per-hunk evidence                   |
| `scripts/docs-proposal/propose.sh`                 | Job `propose` entry point                                    |
| `scripts/docs-proposal/publish.sh`                 | Job `publish` entry point                                    |
| `scripts/docs-proposal/test.sh`                    | Bash tests for `lib.sh` and `publish.sh` with stubbed `gh`   |
| `.github/workflows/docs-proposal.yml`              | The proposal workflow                                        |
| `.github/workflows/format.yml`                     | Add a job that lints and tests the proposal scripts          |
| `scripts/README.md`, `.gitignore`, the design spec | Prerequisites, ignored work dir, small spec corrections      |

---

### Task 1: Diff parser and hunk listing

**Files:**

- Create: `scripts/docs-proposal/hunks.mjs`
- Test: `scripts/docs-proposal/hunks.test.mjs`

**Interfaces:**

- Produces:
  - `parseDiff(text: string): File[]` where
    `File = { path: string, status: "modified"|"added"|"deleted", binary: boolean, hunks: Hunk[] }`,
    `Hunk = { lines: Line[] }`,
    `Line = { type: " "|"-"|"+", text: string, oldNo: number|null, newNo: number|null }`.
  - `collectHunks(files: File[]): { id: string, path: string, status: string, hunk: Hunk }[]`
    (binary files skipped; ids are `<path>#<8 hex>`, with `-2`, `-3` … on collision).
  - `listHunks(files: File[]): { id, path, status, removed: string[], added: string[] }[]`.
  - CLI: `node hunks.mjs list < diff` prints `listHunks` JSON.

- [ ] **Step 1: Write the failing tests**

```js
// scripts/docs-proposal/hunks.test.mjs
import assert from 'node:assert/strict'
import { test } from 'node:test'

import { collectHunks, listHunks, parseDiff } from './hunks.mjs'

const MODIFIED = [
  'diff --git a/docs/guide/cli.md b/docs/guide/cli.md',
  'index 1111111..2222222 100644',
  '--- a/docs/guide/cli.md',
  '+++ b/docs/guide/cli.md',
  '@@ -10,4 +10,4 @@ heading',
  ' keep one',
  '-old flag',
  '+new flag',
  ' keep two',
  '',
].join('\n')

test('parseDiff numbers context, removed, and added lines', () => {
  const [file] = parseDiff(MODIFIED)
  assert.equal(file.path, 'docs/guide/cli.md', 'should take the b/ path')
  assert.equal(file.status, 'modified', 'should default to modified')
  assert.deepEqual(
    file.hunks[0].lines,
    [
      { type: ' ', text: 'keep one', oldNo: 10, newNo: 10 },
      { type: '-', text: 'old flag', oldNo: 11, newNo: null },
      { type: '+', text: 'new flag', oldNo: null, newNo: 11 },
      { type: ' ', text: 'keep two', oldNo: 12, newNo: 12 },
    ],
    'should track old and new line numbers'
  )
})

test('parseDiff records added, deleted, and binary files', () => {
  const diff = [
    'diff --git a/docs/guide/new.md b/docs/guide/new.md',
    'new file mode 100644',
    '--- /dev/null',
    '+++ b/docs/guide/new.md',
    '@@ -0,0 +1 @@',
    '+hello',
    'diff --git a/docs/guide/gone.md b/docs/guide/gone.md',
    'deleted file mode 100644',
    '--- a/docs/guide/gone.md',
    '+++ /dev/null',
    '@@ -1 +0,0 @@',
    '-bye',
    'diff --git a/docs/guide/logo.png b/docs/guide/logo.png',
    'Binary files a/docs/guide/logo.png and b/docs/guide/logo.png differ',
    '',
  ].join('\n')
  const files = parseDiff(diff)
  assert.deepEqual(
    files.map(({ path, status, binary }) => ({ path, status, binary })),
    [
      { path: 'docs/guide/new.md', status: 'added', binary: false },
      { path: 'docs/guide/gone.md', status: 'deleted', binary: false },
      { path: 'docs/guide/logo.png', status: 'modified', binary: true },
    ],
    'should classify each file'
  )
  assert.deepEqual(
    files[0].hunks[0].lines,
    [{ type: '+', text: 'hello', oldNo: null, newNo: 1 }],
    'should start a new file at line 1'
  )
})

test('parseDiff skips no-newline markers', () => {
  const diff = MODIFIED.replace(
    ' keep two\n',
    ' keep two\n\\ No newline at end of file\n'
  )
  const [file] = parseDiff(diff)
  assert.equal(file.hunks[0].lines.length, 4, 'should ignore the marker line')
})

test('collectHunks gives stable ids and skips binary files', () => {
  const first = collectHunks(parseDiff(MODIFIED))
  const second = collectHunks(
    parseDiff(MODIFIED.replace('@@ -10,4 +10,4', '@@ -20,4 +20,4'))
  )
  assert.equal(first.length, 1, 'should collect the single hunk')
  assert.match(
    first[0].id,
    /^docs\/guide\/cli\.md#[0-9a-f]{8}$/,
    'should use path#hash'
  )
  assert.equal(first[0].id, second[0].id, 'should not depend on line numbers')
})

test('collectHunks suffixes duplicate ids', () => {
  const twice = MODIFIED + MODIFIED.split('\n').slice(4).join('\n')
  const ids = collectHunks(parseDiff(twice)).map((entry) => entry.id)
  assert.equal(ids.length, 2, 'should collect both hunks')
  assert.equal(ids[1], `${ids[0]}-2`, 'should suffix the repeated id')
})

test('listHunks exposes removed and added text', () => {
  const [entry] = listHunks(parseDiff(MODIFIED))
  assert.deepEqual(entry.removed, ['old flag'], 'should list removed lines')
  assert.deepEqual(entry.added, ['new flag'], 'should list added lines')
  assert.equal(entry.status, 'modified', 'should carry the file status')
})
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `node --test scripts/docs-proposal/hunks.test.mjs`
Expected: FAIL with `Cannot find module` for `hunks.mjs`.

- [ ] **Step 3: Write the implementation**

```js
// scripts/docs-proposal/hunks.mjs
// Parses a unified documentation diff into hunks and builds the per-hunk
// review that cites Copilot evidence and offers a suggestion reverting each
// hunk. Dependency-free so it runs on any runner with Node.
import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const FILE_HEADER = /^diff --git a\/(.+) b\/(.+)$/
const HUNK_HEADER = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/

export function parseDiff(text) {
  const files = []
  let file = null
  let hunk = null
  let oldNo = 0
  let newNo = 0
  for (const raw of text.split('\n')) {
    const fileHeader = FILE_HEADER.exec(raw)
    if (fileHeader) {
      file = {
        path: fileHeader[2],
        status: 'modified',
        binary: false,
        hunks: [],
      }
      files.push(file)
      hunk = null
      continue
    }
    if (!file) {
      continue
    }
    const hunkHeader = HUNK_HEADER.exec(raw)
    if (hunkHeader) {
      oldNo = Number(hunkHeader[1])
      newNo = Number(hunkHeader[2])
      hunk = { lines: [] }
      file.hunks.push(hunk)
      continue
    }
    if (!hunk) {
      if (raw.startsWith('new file mode')) {
        file.status = 'added'
      } else if (raw.startsWith('deleted file mode')) {
        file.status = 'deleted'
      } else if (raw.startsWith('Binary files ')) {
        file.binary = true
      }
      continue
    }
    const type = raw[0]
    const lineText = raw.slice(1)
    if (type === ' ') {
      hunk.lines.push({ type, text: lineText, oldNo: oldNo++, newNo: newNo++ })
    } else if (type === '-') {
      hunk.lines.push({ type, text: lineText, oldNo: oldNo++, newNo: null })
    } else if (type === '+') {
      hunk.lines.push({ type, text: lineText, oldNo: null, newNo: newNo++ })
    }
  }
  return files
}

function hashHunk(path, hunk) {
  const hash = createHash('sha256')
  hash.update(path)
  for (const line of hunk.lines) {
    if (line.type !== ' ') {
      hash.update(`\n${line.type}${line.text}`)
    }
  }
  return `${path}#${hash.digest('hex').slice(0, 8)}`
}

export function collectHunks(files) {
  const seen = new Map()
  const entries = []
  for (const file of files) {
    if (file.binary) {
      continue
    }
    for (const hunk of file.hunks) {
      const base = hashHunk(file.path, hunk)
      const count = (seen.get(base) ?? 0) + 1
      seen.set(base, count)
      const id = count === 1 ? base : `${base}-${count}`
      entries.push({ id, path: file.path, status: file.status, hunk })
    }
  }
  return entries
}

export function listHunks(files) {
  return collectHunks(files).map(({ id, path, status, hunk }) => ({
    id,
    path,
    status,
    removed: hunk.lines
      .filter((line) => line.type === '-')
      .map((line) => line.text),
    added: hunk.lines
      .filter((line) => line.type === '+')
      .map((line) => line.text),
  }))
}

function main([command]) {
  const files = parseDiff(readFileSync(0, 'utf8'))
  if (command === 'list') {
    process.stdout.write(`${JSON.stringify(listHunks(files), null, 2)}\n`)
    return
  }
  process.stderr.write(
    'usage: hunks.mjs list | review <evidence.json> <commit-sha>\n'
  )
  process.exitCode = 2
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `node --test scripts/docs-proposal/hunks.test.mjs`
Expected: PASS, 6 tests.

- [ ] **Step 5: Commit**

```bash
git add scripts/docs-proposal/hunks.mjs scripts/docs-proposal/hunks.test.mjs
git commit -S --signoff -m "Add documentation diff hunk parser"
```

---

### Task 2: Per-hunk review builder

**Files:**

- Modify: `scripts/docs-proposal/hunks.mjs`
- Test: `scripts/docs-proposal/hunks.test.mjs`

**Interfaces:**

- Consumes: `parseDiff`, `collectHunks` from Task 1.
- Produces:
  - `revertSuggestion(hunk: Hunk): { startLine: number, line: number, body: string[] } | null`.
  - `indexEvidence(raw: unknown): Map<string, { source: string, reason: string }>`.
  - `buildReview(files: File[], evidence: Map, commitId: string): { commit_id, event: "COMMENT", body: string, comments: object[] }`.
  - CLI: `node hunks.mjs review <evidence.json> <commit-sha> < diff` prints the
    review payload for `POST /repos/{owner}/{repo}/pulls/{number}/reviews`. A
    missing or malformed evidence file is treated as no evidence.

- [ ] **Step 1: Write the failing tests** (append to `hunks.test.mjs`, and
      extend the import to
      `import { buildReview, collectHunks, indexEvidence, listHunks, parseDiff, revertSuggestion } from "./hunks.mjs";`)

`````js
function hunkOf(lines, oldStart = 1, newStart = 1) {
  const diff = [
    'diff --git a/docs/guide/a.md b/docs/guide/a.md',
    '--- a/docs/guide/a.md',
    '+++ b/docs/guide/a.md',
    `@@ -${oldStart} +${newStart} @@`,
    ...lines,
    '',
  ].join('\n')
  return parseDiff(diff)[0].hunks[0]
}

test('revertSuggestion restores replaced lines', () => {
  const suggestion = revertSuggestion(hunkOf([' a', '-b', '+B', '+B2', ' c']))
  assert.deepEqual(
    suggestion,
    { startLine: 2, line: 3, body: ['b'] },
    'should span the new lines and restore the old one'
  )
})

test('revertSuggestion deletes pure insertions', () => {
  const suggestion = revertSuggestion(hunkOf([' a', '+new', ' c']))
  assert.deepEqual(
    suggestion,
    { startLine: 2, line: 2, body: [] },
    'should suggest an empty body'
  )
})

test('revertSuggestion spans several change groups with their context', () => {
  const suggestion = revertSuggestion(hunkOf(['-a', '+A', ' mid', '-c', '+C']))
  assert.deepEqual(
    suggestion,
    { startLine: 1, line: 3, body: ['a', 'mid', 'c'] },
    'should keep the middle context line'
  )
})

test('revertSuggestion anchors pure deletions on the previous context line', () => {
  const suggestion = revertSuggestion(hunkOf([' a', '-gone', ' c'], 4, 4))
  assert.deepEqual(
    suggestion,
    { startLine: 4, line: 4, body: ['a', 'gone'] },
    'should restore the deleted line after its anchor'
  )
})

test('revertSuggestion anchors leading deletions on the next context line', () => {
  const suggestion = revertSuggestion(hunkOf(['-gone', ' a']))
  assert.deepEqual(
    suggestion,
    { startLine: 1, line: 1, body: ['gone', 'a'] },
    'should restore the deleted line before its anchor'
  )
})

test('revertSuggestion returns null without an anchor', () => {
  assert.equal(
    revertSuggestion(hunkOf(['-only'])),
    null,
    'should not invent an anchor'
  )
})

test('indexEvidence keeps valid entries and sanitizes text', () => {
  const evidence = indexEvidence({
    hunks: [
      {
        id: 'x#1',
        source: 'crates/a.rs:4',
        reason: 'Adds `--flag`\nfor @team',
      },
      { id: 7, source: 'ignored' },
      { id: 'x#2' },
      'junk',
    ],
  })
  assert.deepEqual(
    [...evidence],
    [['x#1', { source: 'crates/a.rs:4', reason: 'Adds --flag for @​team' }]],
    'should drop invalid entries and strip backticks, newlines, and mentions'
  )
  assert.equal(indexEvidence(null).size, 0, 'should accept missing evidence')
})

test('buildReview cites evidence and suggests reverts', () => {
  const files = parseDiff(
    [
      'diff --git a/docs/guide/a.md b/docs/guide/a.md',
      '--- a/docs/guide/a.md',
      '+++ b/docs/guide/a.md',
      '@@ -1,3 +1,3 @@',
      ' a',
      '-uses ```fence```',
      '+B',
      ' c',
      '@@ -10 +10,2 @@',
      ' j',
      '+k',
      '',
    ].join('\n')
  )
  const [first, second] = collectHunks(files)
  const evidence = indexEvidence({
    hunks: [
      { id: first.id, source: 'crates/a.rs:9', reason: 'Renames the flag.' },
    ],
  })
  const review = buildReview(files, evidence, 'abc123')

  assert.equal(review.commit_id, 'abc123', 'should pin the reviewed commit')
  assert.equal(review.event, 'COMMENT', 'should not approve or request changes')
  assert.equal(review.comments.length, 2, 'should comment on every hunk')
  assert.deepEqual(
    review.comments[0],
    {
      path: 'docs/guide/a.md',
      line: 2,
      side: 'RIGHT',
      body: [
        '**Source:** `crates/a.rs:9` — Renames the flag.',
        '',
        'Apply this suggestion to revert the hunk.',
        '',
        '````suggestion',
        'uses ```fence```',
        '````',
      ].join('\n'),
    },
    'should escalate the fence and omit start_line for one line'
  )
  assert.match(
    review.comments[1].body,
    /none cited/,
    'should flag hunks without evidence'
  )
  assert.match(
    review.comments[1].body,
    /```suggestion\n```$/,
    'should suggest deleting an insertion'
  )
  assert.equal(second.id, collectHunks(files)[1].id, 'should be deterministic')
})

test('buildReview lists binary and unanchored changes in the body', () => {
  const files = parseDiff(
    [
      'diff --git a/docs/guide/gone.md b/docs/guide/gone.md',
      'deleted file mode 100644',
      '@@ -1 +0,0 @@',
      '-bye',
      'diff --git a/docs/guide/logo.png b/docs/guide/logo.png',
      'Binary files a/docs/guide/logo.png and b/docs/guide/logo.png differ',
      '',
    ].join('\n')
  )
  const review = buildReview(files, new Map(), 'abc123')
  assert.equal(review.comments.length, 0, 'should not post line comments')
  assert.match(
    review.body,
    /`docs\/guide\/gone\.md`: file deleted/,
    'should note the deletion'
  )
  assert.match(
    review.body,
    /`docs\/guide\/logo\.png`: binary change/,
    'should note the binary file'
  )
})

test('buildReview adds start_line for multi-line spans', () => {
  const files = parseDiff(
    [
      'diff --git a/docs/index.md b/docs/index.md',
      '@@ -1 +1,2 @@',
      '-a',
      '+b',
      '+c',
      '',
    ].join('\n')
  )
  const [comment] = buildReview(files, new Map(), 'abc123').comments
  assert.equal(comment.start_line, 1, 'should start at the first new line')
  assert.equal(
    comment.start_side,
    'RIGHT',
    'should anchor the start on the new side'
  )
  assert.equal(comment.line, 2, 'should end at the last new line')
})
`````

- [ ] **Step 2: Run tests to verify they fail**

Run: `node --test scripts/docs-proposal/hunks.test.mjs`
Expected: FAIL with `does not provide an export named 'buildReview'`.

- [ ] **Step 3: Implement** — add to `hunks.mjs` after `listHunks`:

```js
const MAX_EVIDENCE_LENGTH = 300

const REVIEW_INTRO =
  'Copilot evidence for each proposed hunk. Leave a hunk to accept it, or apply its suggestion to revert it. Verify every cited source before merging.'

export function revertSuggestion(hunk) {
  const { lines } = hunk
  const first = lines.findIndex((line) => line.type !== ' ')
  const last = lines.findLastIndex((line) => line.type !== ' ')
  if (first === -1) {
    return null
  }
  const span = lines.slice(first, last + 1)
  const newSide = span.filter((line) => line.type !== '-')
  const oldText = span
    .filter((line) => line.type !== '+')
    .map((line) => line.text)
  if (newSide.length > 0) {
    return {
      startLine: newSide[0].newNo,
      line: newSide.at(-1).newNo,
      body: oldText,
    }
  }
  const before = lines[first - 1]
  if (before?.type === ' ') {
    return {
      startLine: before.newNo,
      line: before.newNo,
      body: [before.text, ...oldText],
    }
  }
  const after = lines[last + 1]
  if (after?.type === ' ') {
    return {
      startLine: after.newNo,
      line: after.newNo,
      body: [...oldText, after.text],
    }
  }
  return null
}

function sanitize(value) {
  if (typeof value !== 'string') {
    return ''
  }
  return value
    .replace(/[`\r\n]+/g, ' ')
    .replace(/@/g, '@​')
    .replace(/\s+/g, ' ')
    .trim()
    .slice(0, MAX_EVIDENCE_LENGTH)
}

export function indexEvidence(raw) {
  const evidence = new Map()
  const items = Array.isArray(raw?.hunks) ? raw.hunks : []
  for (const item of items) {
    if (typeof item?.id !== 'string') {
      continue
    }
    const source = sanitize(item.source)
    const reason = sanitize(item.reason)
    if (source || reason) {
      evidence.set(item.id, { source, reason })
    }
  }
  return evidence
}

function fenceFor(body) {
  const longestRun = Math.max(
    0,
    ...body.map((text) =>
      Math.max(0, ...(text.match(/`+/g) ?? []).map((run) => run.length))
    )
  )
  return '`'.repeat(Math.max(3, longestRun + 1))
}

function commentBody(cited, body) {
  const source = cited?.source
    ? `\`${cited.source}\``
    : 'none cited — verify this hunk against the merged code.'
  const reason = cited?.reason ? ` — ${cited.reason}` : ''
  const fence = fenceFor(body)
  const suggestion =
    body.length > 0
      ? `${fence}suggestion\n${body.join('\n')}\n${fence}`
      : `${fence}suggestion\n${fence}`
  return [
    `**Source:** ${source}${reason}`,
    '',
    'Apply this suggestion to revert the hunk.',
    '',
    suggestion,
  ].join('\n')
}

export function buildReview(files, evidence, commitId) {
  const comments = []
  const notes = []
  for (const file of files) {
    if (file.binary) {
      notes.push(
        `- \`${file.path}\`: binary change — review the file directly.`
      )
    }
  }
  for (const { id, path, status, hunk } of collectHunks(files)) {
    const suggestion = revertSuggestion(hunk)
    if (!suggestion) {
      const reason =
        status === 'deleted' ? 'file deleted' : 'no line to anchor a revert'
      notes.push(`- \`${path}\`: ${reason} — revert it manually if needed.`)
      continue
    }
    const comment = {
      path,
      line: suggestion.line,
      side: 'RIGHT',
      body: commentBody(evidence.get(id), suggestion.body),
    }
    if (suggestion.startLine !== suggestion.line) {
      comment.start_line = suggestion.startLine
      comment.start_side = 'RIGHT'
    }
    comments.push(comment)
  }
  const body =
    notes.length > 0
      ? `${REVIEW_INTRO}\n\nChanges without a suggestion:\n\n${notes.join('\n')}`
      : REVIEW_INTRO
  return { commit_id: commitId, event: 'COMMENT', body, comments }
}

function readEvidence(path) {
  try {
    return indexEvidence(JSON.parse(readFileSync(path, 'utf8')))
  } catch (error) {
    process.stderr.write(
      `Ignoring unreadable evidence at ${path}: ${error.message}\n`
    )
    return new Map()
  }
}
```

Note: the `comment` object key order in the test is `path, line, side, body`;
`deepEqual` ignores key order, so the construction order above is fine.

Then extend `main`:

```js
function main([command, evidencePath, commitId]) {
  const files = parseDiff(readFileSync(0, 'utf8'))
  if (command === 'list') {
    process.stdout.write(`${JSON.stringify(listHunks(files), null, 2)}\n`)
    return
  }
  if (command === 'review' && evidencePath && commitId) {
    const review = buildReview(files, readEvidence(evidencePath), commitId)
    process.stdout.write(`${JSON.stringify(review, null, 2)}\n`)
    return
  }
  process.stderr.write(
    'usage: hunks.mjs list | review <evidence.json> <commit-sha>\n'
  )
  process.exitCode = 2
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `node --test scripts/docs-proposal/hunks.test.mjs`
Expected: PASS, 16 tests.

- [ ] **Step 5: Commit**

```bash
git add scripts/docs-proposal/hunks.mjs scripts/docs-proposal/hunks.test.mjs
git commit -S --signoff -m "Build per-hunk revert reviews for documentation proposals"
```

---

### Task 3: Shared helpers, prompts, and `propose.sh`

**Files:**

- Create: `scripts/docs-proposal/lib.sh`, `scripts/docs-proposal/prompt.md`,
  `scripts/docs-proposal/evidence-prompt.md`, `scripts/docs-proposal/propose.sh`
- Create: `scripts/docs-proposal/test.sh` (lib tests only in this task)
- Modify: `.gitignore` (add `/.docs-proposal/`)

**Interfaces:**

- Consumes: `node hunks.mjs list` from Task 1.
- Produces (sourced by `publish.sh` in Task 4):
  - `docs_proposal_require_sha <sha>`: exits 1 unless full hex SHA reachable from `origin/main`.
  - `docs_proposal_branch <sha>` → prints `docs/auto/<sha12>`.
  - `docs_proposal_disallowed_paths` (stdin: paths) → prints bad paths, returns 1 if any.
  - `docs_proposal_pr_body <sha> <subject> <origin-pr-or-empty> <rationale-file>` → prints body.
  - `propose.sh <sha> <dir>` writes `<dir>/proposal.patch` (possibly empty),
    `<dir>/rationale.md`, `<dir>/evidence.json`.

- [ ] **Step 1: Write the failing lib tests**

```bash
#!/usr/bin/env bash
# Tests the documentation proposal helpers and publish flow with stubbed gh
# and npm against a throwaway git repository.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/docs-proposal/lib.sh
source "$here/lib.sh"

failures=0

assert_eq() {
  if [ "$1" != "$2" ]; then
    printf 'FAIL: %s\n  expected: %s\n  actual:   %s\n' "$3" "$2" "$1"
    failures=$((failures + 1))
  fi
}

assert_contains() {
  if [[ "$1" != *"$2"* ]]; then
    printf 'FAIL: %s\n  missing: %s\n' "$3" "$2"
    failures=$((failures + 1))
  fi
}

sha=0123456789abcdef0123456789abcdef01234567

assert_eq "$(docs_proposal_branch "$sha")" "docs/auto/0123456789ab" "should derive the branch from the SHA"

bad="$(printf 'docs/guide/a.md\ndocs/index.md\ndocs/superpowers/x.md\nREADME.md\n' | docs_proposal_disallowed_paths || true)"
assert_eq "$bad" $'docs/superpowers/x.md\nREADME.md' "should print only paths outside the allowlist"

if printf 'docs/guide/a.md\ndocs/guide/integrations/b.md\n' | docs_proposal_disallowed_paths >/dev/null; then
  status=0
else
  status=1
fi
assert_eq "$status" 0 "should accept guide and nested guide paths"

rationale="$(mktemp)"
printf -- '- docs/guide/cli.md: documents the new flag.\n' > "$rationale"
body="$(docs_proposal_pr_body "$sha" "Add example flag" 42 "$rationale")"
assert_contains "$body" "<!-- docs-proposal: $sha -->" "should embed the SHA marker"
assert_contains "$body" "merged change $sha (#42): Add example flag" "should link the merge and its PR"
assert_contains "$body" "documents the new flag" "should include the rationale"
: > "$rationale"
body="$(docs_proposal_pr_body "$sha" "Add example flag" "" "$rationale")"
assert_contains "$body" "merged change $sha: Add example flag" "should omit a missing origin PR"
assert_contains "$body" "did not provide a rationale" "should note an empty rationale"
rm -f "$rationale"

if (docs_proposal_require_sha "not-a-sha") 2>/dev/null; then
  status=0
else
  status=1
fi
assert_eq "$status" 1 "should reject a malformed SHA"

# PUBLISH_TESTS

if [ "$failures" -gt 0 ]; then
  printf '%d failure(s)\n' "$failures"
  exit 1
fi
printf 'All documentation proposal script tests passed.\n'
```

- [ ] **Step 2: Run to verify it fails**

Run: `chmod +x scripts/docs-proposal/test.sh && scripts/docs-proposal/test.sh`
Expected: FAIL — `lib.sh: No such file or directory`.

- [ ] **Step 3: Write `lib.sh`**

```bash
# shellcheck shell=bash
# Shared helpers for the documentation proposal scripts. Source this file;
# do not execute it.

DOCS_PROPOSAL_ALLOWED_RE='^docs/(guide/.+|index\.md)$'
DOCS_PROPOSAL_MAX_RATIONALE_BYTES=20000

# Exits unless the argument is a full commit SHA reachable from origin/main.
docs_proposal_require_sha() {
  local sha="$1"
  if ! [[ "$sha" =~ ^[0-9a-f]{40}$ ]]; then
    printf '::error::Expected a full 40-character commit SHA, got %s\n' "$sha" >&2
    exit 1
  fi
  if ! git merge-base --is-ancestor "$sha" origin/main 2>/dev/null; then
    printf '::error::Commit %s is not reachable from origin/main\n' "$sha" >&2
    exit 1
  fi
}

docs_proposal_branch() {
  printf 'docs/auto/%s\n' "${1:0:12}"
}

# Reads changed paths on stdin, prints those outside the allowlist, and
# returns 1 when any were found.
docs_proposal_disallowed_paths() {
  local path
  local found=0
  while IFS= read -r path; do
    if [ -n "$path" ] && ! [[ "$path" =~ $DOCS_PROPOSAL_ALLOWED_RE ]]; then
      printf '%s\n' "$path"
      found=1
    fi
  done
  return "$found"
}

docs_proposal_pr_body() {
  local sha="$1"
  local subject="$2"
  local origin_pr="$3"
  local rationale="$4"
  local origin=""
  if [ -n "$origin_pr" ]; then
    origin=" (#$origin_pr)"
  fi
  printf '<!-- docs-proposal: %s -->\n' "$sha"
  printf 'Proposed documentation update for merged change %s%s: %s\n\n' "$sha" "$origin" "$subject"
  printf 'Generated by GitHub Copilot CLI in the docs-proposal workflow. A maintainer must verify every hunk against the merged code before merging. Each hunk has a review comment citing its source and a suggestion that reverts it.\n\n'
  printf '## Rationale\n\n'
  if [ -s "$rationale" ]; then
    head -c "$DOCS_PROPOSAL_MAX_RATIONALE_BYTES" "$rationale"
    printf '\n'
  else
    printf '_Copilot did not provide a rationale._\n'
  fi
}
```

- [ ] **Step 4: Run lib tests to verify they pass**

Run: `scripts/docs-proposal/test.sh`
Expected: `All documentation proposal script tests passed.`

- [ ] **Step 5: Write the prompts**

`scripts/docs-proposal/prompt.md`:

```markdown
# Documentation proposal

You are updating the public Trusted Server documentation after a commit merged
to `main`. The merged commit and the table mapping implementation sources to
documentation pages appear at the end of this file.

## Task

1. Inspect the merged change with `git show --stat <commit>` and
   `git show <commit>`.
2. Decide whether it changes behavior that `docs/index.md` or a page under
   `docs/guide/` describes: configuration fields and defaults, routes, adapter
   support, integrations, auction behavior, CLI commands and flags, browser
   bundles, or verification commands. Use the Sources of truth table to find
   the affected pages.
3. If no reader-facing documentation is affected, change nothing and write
   `.docs-proposal/rationale.md` containing one sentence explaining why.
4. Otherwise edit only the affected pages, then write
   `.docs-proposal/rationale.md` with one bullet per edited page naming the
   page, what changed, and the merged source file that justifies it.

## Rules

- Describe behavior the merged code ships. Do not document intent, roadmap
  items, or unmerged work.
- Read the merged source before writing. Every statement you add must be
  verifiable in the code at this commit.
- Make the smallest edit that keeps the page accurate. Do not restyle,
  reorder, or rewrite unaffected text.
- Use only fictional values: `example.com` domains and placeholder secrets.
  Never copy real domains, customer names, credentials, or service IDs.
- Keep existing links and anchors working, and link to other pages with
  relative paths.
- Do not edit files outside `docs/guide/`, `docs/index.md`, and
  `.docs-proposal/`. Do not commit, push, or create branches.
```

`scripts/docs-proposal/evidence-prompt.md`:

````markdown
# Documentation proposal evidence

The hunks at the end of this file are proposed edits to the Trusted Server
documentation for the merged commit named there. For each hunk, find the merged
source that justifies it.

Write `.docs-proposal/evidence.json` with exactly this shape, and change no
other file:

```json
{
  "hunks": [
    { "id": "<hunk id>", "source": "<path>:<line>", "reason": "<one sentence>" }
  ]
}
```

- Copy `id` exactly from the hunk list.
- `source` is a repository-relative path and line at the merged commit that
  shows the documented behavior.
- `reason` is one sentence a reviewer can check against that source.
- If no source justifies a hunk, omit it. The reviewer will be told the hunk
  has no cited source.
````

- [ ] **Step 6: Write `propose.sh`**

````bash
#!/usr/bin/env bash
# Asks Copilot CLI to propose documentation edits for one merged commit.
#
# Usage: scripts/docs-proposal/propose.sh <merge-sha> <work-dir>
# Writes proposal.patch (empty when nothing changes), rationale.md, and
# evidence.json to <work-dir>, which must be inside the repository.
set -euo pipefail

sha="$1"
work_dir="$2"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(git rev-parse --show-toplevel)"
spec="$root/docs/superpowers/specs/2026-08-19-documentation-refresh-design.md"
# shellcheck source=scripts/docs-proposal/lib.sh
source "$here/lib.sh"

cd "$root"
docs_proposal_require_sha "$sha"
mkdir -p "$work_dir"
work_dir="$(cd "$work_dir" && pwd)"
relative_dir="${work_dir#"$root"/}"

run_copilot() {
  copilot -p "Read $relative_dir/$1 and follow its instructions exactly." \
    --no-ask-user \
    --allow-tool=write \
    --allow-tool='shell(git show:*)' \
    --allow-tool='shell(git diff:*)' \
    --allow-tool='shell(git log:*)' \
    --allow-tool='shell(git grep:*)' \
    --deny-tool='shell(git push)' \
    --deny-tool='shell(git commit)'
}

{
  cat "$here/prompt.md"
  printf '\n## Merged change\n\n- Commit: %s\n- Subject: %s\n\n' "$sha" "$(git log -1 --format=%s "$sha")"
  printf '## Sources of truth\n\n'
  awk '/^## Sources of truth/ { found = 1; next } /^## / { found = 0 } found' "$spec"
} > "$work_dir/prompt.md"
run_copilot prompt.md

# Normalize Copilot's edits before hunks are identified so ids stay valid
# after the publish job's format check.
(cd docs && npm ci && npm run format:write)

git add -A -- docs
git diff --cached --no-renames --binary "$sha" -- docs > "$work_dir/proposal.patch"
touch "$work_dir/rationale.md"
if [ ! -s "$work_dir/proposal.patch" ]; then
  printf '{}\n' > "$work_dir/evidence.json"
  printf 'No documentation changes proposed for %s.\n' "$sha"
  exit 0
fi

{
  cat "$here/evidence-prompt.md"
  printf '\n## Merged change\n\n- Commit: %s\n\n## Hunks\n\n```json\n' "$sha"
  git diff --cached --no-renames "$sha" -- docs | node "$here/hunks.mjs" list
  printf '```\n'
} > "$work_dir/evidence-prompt.md"
run_copilot evidence-prompt.md
if [ ! -f "$work_dir/evidence.json" ]; then
  printf '{}\n' > "$work_dir/evidence.json"
fi
````

- [ ] **Step 7: Ignore the work dir, lint, and commit**

Append `/.docs-proposal/` to `.gitignore` under a comment
`# Documentation proposal workflow scratch space`.

Run: `chmod +x scripts/docs-proposal/propose.sh && shellcheck scripts/docs-proposal/*.sh` (skip locally if
`shellcheck` is not installed; CI runs it in Task 5).
Run: `scripts/docs-proposal/test.sh` — Expected: pass.

```bash
git add .gitignore scripts/docs-proposal/lib.sh scripts/docs-proposal/prompt.md scripts/docs-proposal/evidence-prompt.md scripts/docs-proposal/propose.sh scripts/docs-proposal/test.sh
git commit -S --signoff -m "Add Copilot documentation proposal script"
```

---

### Task 4: `publish.sh` with stubbed integration tests

**Files:**

- Create: `scripts/docs-proposal/publish.sh`
- Modify: `scripts/docs-proposal/test.sh` (replace the `# PUBLISH_TESTS` line)

**Interfaces:**

- Consumes: `lib.sh` (Task 3), `node hunks.mjs review` (Task 2), files from `propose.sh`.
- Produces: `publish.sh <sha> <work-dir>`; env `GH_TOKEN`. `gh` calls used:
  - `gh pr list --head <branch> --state all --json number,state --jq '.[0] | select(.) | "\(.number) \(.state)"'`
  - `gh pr close <n> --comment <text> --delete-branch`
  - `gh api repos/{owner}/{repo}/commits/<sha>/pulls --jq '.[0].number // empty'`
  - `gh pr create --base main --head <branch> --title <t> --body-file <f>` (prints URL)
  - `gh pr edit <n> --title <t> --body-file <f>`
  - `gh api --method POST repos/{owner}/{repo}/pulls/<n>/reviews --input <file>`

- [ ] **Step 1: Write the failing publish tests** — replace `# PUBLISH_TESTS`
      in `test.sh` with:

```bash
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=Example GIT_AUTHOR_EMAIL=dev@example.com
export GIT_COMMITTER_NAME=Example GIT_COMMITTER_EMAIL=dev@example.com

mkdir -p "$tmp/bin"
cat > "$tmp/bin/gh" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$STUB_LOG"
case "$1 $2" in
  "pr list") printf '%s' "${STUB_PR:-}" ;;
  "pr create") printf 'https://github.com/example/repo/pull/7\n' ;;
  "api repos/{owner}/{repo}/commits/"*) printf '%s' "${STUB_ORIGIN_PR:-}" ;;
  "api --method")
    for arg in "$@"; do last="$arg"; done
    cp "$last" "$STUB_REVIEW"
    ;;
esac
STUB
printf '#!/usr/bin/env bash\nprintf "npm %%s\\n" "$*" >> "$STUB_LOG"\n' > "$tmp/bin/npm"
chmod +x "$tmp/bin/gh" "$tmp/bin/npm"
export PATH="$tmp/bin:$PATH" STUB_LOG="$tmp/gh.log" STUB_REVIEW="$tmp/review.json"

git init -q --bare -b main "$tmp/origin.git"
git clone -q "$tmp/origin.git" "$tmp/work" 2>/dev/null
cd "$tmp/work"
mkdir -p docs/guide
printf 'line one\nline two\nline three\n' > docs/guide/cli.md
git add docs && git commit -q -m "Seed docs" && git push -q origin HEAD:main
merge_sha="$(git rev-parse HEAD)"
branch="$(docs_proposal_branch "$merge_sha")"

# Each publish run leaves HEAD on its proposal commit, so every patch starts
# from a clean checkout of the merge commit.
make_patch() {
  git reset -q --hard "$merge_sha" && git clean -qfd
  rm -rf "$tmp/proposal" && mkdir -p "$tmp/proposal"
  if [ -n "$1" ]; then
    mkdir -p "$(dirname "$1")" && printf 'proposed\n' >> "$1"
    git add -A && git diff --cached --binary "$merge_sha" > "$tmp/proposal/proposal.patch"
    git reset -q --hard "$merge_sha" && git clean -qfd
  else
    : > "$tmp/proposal/proposal.patch"
  fi
  printf -- '- docs/guide/cli.md: example rationale.\n' > "$tmp/proposal/rationale.md"
  printf '{}\n' > "$tmp/proposal/evidence.json"
}

run_publish() {
  : > "$STUB_LOG"
  rm -f "$STUB_REVIEW"
  if "$here/publish.sh" "$merge_sha" "$tmp/proposal" > "$tmp/out.log" 2>&1; then
    printf '0'
  else
    printf '1'
  fi
}

remote_has_branch() {
  if git ls-remote --exit-code -q origin "refs/heads/$branch" >/dev/null; then printf 'yes'; else printf 'no'; fi
}

make_patch ""
assert_eq "$(STUB_PR="" run_publish)" 0 "should succeed on an empty proposal"
assert_eq "$(grep -c 'pr create\|pr close' "$STUB_LOG" || true)" 0 "should not open or close a PR for an empty diff"

make_patch ""
assert_eq "$(STUB_PR="7 OPEN" run_publish)" 0 "should succeed when closing a stale proposal"
assert_contains "$(cat "$STUB_LOG")" "pr close 7" "should close the open proposal for an empty diff"

make_patch docs/superpowers/notes.md
assert_eq "$(STUB_PR="" run_publish)" 1 "should fail for a disallowed path"
assert_contains "$(cat "$tmp/out.log")" "docs/superpowers/notes.md" "should name the disallowed path"
assert_eq "$(remote_has_branch)" no "should not push a disallowed proposal"

make_patch docs/guide/cli.md
assert_eq "$(STUB_PR="" STUB_ORIGIN_PR=42 run_publish)" 0 "should publish a valid proposal"
assert_eq "$(remote_has_branch)" yes "should push the proposal branch"
assert_contains "$(cat "$STUB_LOG")" "pr create --base main --head $branch" "should open a PR from the proposal branch"
assert_contains "$(cat "$STUB_LOG")" "npm run build" "should run the docs build"
assert_contains "$(cat "$STUB_REVIEW")" '"path": "docs/guide/cli.md"' "should post a review for the hunk"

assert_eq "$(STUB_PR="7 OPEN" run_publish)" 0 "should succeed on an unchanged retry"
assert_eq "$(grep -c 'pr edit\|pr create\|reviews' "$STUB_LOG" || true)" 0 "should not re-post an unchanged proposal"

make_patch docs/index.md
assert_eq "$(STUB_PR="7 OPEN" run_publish)" 0 "should update a changed proposal"
assert_contains "$(cat "$STUB_LOG")" "pr edit 7" "should edit the existing PR"
assert_contains "$(cat "$STUB_REVIEW")" '"path": "docs/index.md"' "should review the new hunk"

assert_eq "$(STUB_PR="7 CLOSED" run_publish)" 0 "should succeed for a closed proposal"
assert_eq "$(grep -c 'pr edit\|pr create' "$STUB_LOG" || true)" 0 "should never reopen or recreate a closed proposal"

cd "$here"
```

Note: `make_patch docs/index.md` creates `docs/index.md` (an added file) so the
"changed proposal" case diffs a different tree than the pushed branch.

- [ ] **Step 2: Run to verify it fails**

Run: `scripts/docs-proposal/test.sh`
Expected: FAIL lines such as `should succeed on an empty proposal` because
`publish.sh` does not exist.

- [ ] **Step 3: Write `publish.sh`**

```bash
#!/usr/bin/env bash
# Validates a documentation proposal and opens or updates its pull request.
#
# Usage: scripts/docs-proposal/publish.sh <merge-sha> <work-dir>
# <work-dir> holds proposal.patch, rationale.md, and evidence.json from
# propose.sh. Requires GH_TOKEN with contents and pull-requests write access.
set -euo pipefail

sha="$1"
work_dir="$(cd "$2" && pwd)"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(git rev-parse --show-toplevel)"
# shellcheck source=scripts/docs-proposal/lib.sh
source "$here/lib.sh"

cd "$root"
docs_proposal_require_sha "$sha"
branch="$(docs_proposal_branch "$sha")"

existing="$(gh pr list --head "$branch" --state all --json number,state --jq '.[0] | select(.) | "\(.number) \(.state)"')"
pr_number="${existing%% *}"
pr_state="${existing#* }"
if [ -n "$existing" ] && [ "$pr_state" != "OPEN" ]; then
  printf 'Proposal #%s for %s is %s; not reopening it.\n' "$pr_number" "$sha" "$pr_state"
  exit 0
fi

if [ ! -s "$work_dir/proposal.patch" ]; then
  printf 'No documentation changes proposed for %s.\n' "$sha"
  if [ -n "$existing" ]; then
    gh pr close "$pr_number" --delete-branch \
      --comment "A rerun for $sha proposed no documentation changes."
  fi
  exit 0
fi

git switch --quiet --detach "$sha"
git apply --index "$work_dir/proposal.patch"
if ! disallowed="$(git diff --cached --name-only --no-renames "$sha" | docs_proposal_disallowed_paths)"; then
  printf '::error::Proposal changes paths outside docs/guide/** and docs/index.md:\n%s\n' "$disallowed"
  exit 1
fi

(cd docs && npm ci && npm run lint && npm run format && npm run build)

git -c user.name='github-actions[bot]' \
  -c user.email='41898282+github-actions[bot]@users.noreply.github.com' \
  commit --quiet --no-verify \
  -m "Propose documentation updates for ${sha:0:12}" \
  -m "Generated by the docs-proposal workflow from $sha."
head_sha="$(git rev-parse HEAD)"

remote_tree=""
if git fetch --quiet origin "refs/heads/$branch" 2>/dev/null; then
  remote_tree="$(git rev-parse 'FETCH_HEAD^{tree}')"
fi
if [ -n "$existing" ] && [ "$remote_tree" = "$(git rev-parse 'HEAD^{tree}')" ]; then
  printf 'Proposal #%s for %s is unchanged.\n' "$pr_number" "$sha"
  exit 0
fi
git push --quiet --force origin "HEAD:refs/heads/$branch"

subject="$(git log -1 --format=%s "$sha")"
origin_pr="$(gh api "repos/{owner}/{repo}/commits/$sha/pulls" --jq '.[0].number // empty' || true)"
title="$(printf 'Update documentation for %s' "$subject" | cut -c1-200)"
docs_proposal_pr_body "$sha" "$subject" "$origin_pr" "$work_dir/rationale.md" > "$work_dir/pr-body.md"
if [ -n "$existing" ]; then
  gh pr edit "$pr_number" --title "$title" --body-file "$work_dir/pr-body.md"
else
  pr_url="$(gh pr create --base main --head "$branch" --title "$title" --body-file "$work_dir/pr-body.md")"
  pr_number="${pr_url##*/}"
fi

git diff --no-renames "$sha" "$head_sha" -- docs \
  | node "$here/hunks.mjs" review "$work_dir/evidence.json" "$head_sha" > "$work_dir/review.json"
gh api --method POST "repos/{owner}/{repo}/pulls/$pr_number/reviews" --input "$work_dir/review.json" > /dev/null
printf 'Published proposal #%s for %s.\n' "$pr_number" "$sha"
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `chmod +x scripts/docs-proposal/publish.sh && scripts/docs-proposal/test.sh`
Expected: `All documentation proposal script tests passed.`

- [ ] **Step 5: Commit**

```bash
git add scripts/docs-proposal/publish.sh scripts/docs-proposal/test.sh
git commit -S --signoff -m "Publish documentation proposals as per-merge pull requests"
```

---

### Task 5: Workflow, CI coverage, and operator docs

**Files:**

- Create: `.github/workflows/docs-proposal.yml`
- Modify: `.github/workflows/format.yml` (new job), `scripts/README.md` (table rows + prerequisites),
  `docs/superpowers/specs/2026-10-01-docs-proposal-automation-design.md` (corrections below)

**Interfaces:**

- Consumes: `propose.sh <sha> .docs-proposal`, `publish.sh <sha> .docs-proposal`.

- [ ] **Step 1: Write `docs-proposal.yml`**

```yaml
name: Documentation proposal

# Proposes documentation edits with Copilot CLI after code merges to main and
# opens one reviewable pull request per merge. See
# docs/superpowers/specs/2026-10-01-docs-proposal-automation-design.md.
on:
  push:
    branches: [main]
    paths:
      - 'crates/**'
      - 'trusted-server.example.toml'
      - 'edgezero.toml'
      - 'fastly.toml'
      - '.cargo/config.toml'
      - 'scripts/**'
      - '!scripts/docs-proposal/**'
  workflow_dispatch:
    inputs:
      sha:
        description: Full SHA of a commit on main to propose documentation for
        required: true
        type: string

permissions: {}

concurrency:
  group: docs-proposal
  cancel-in-progress: false

env:
  MERGE_SHA: ${{ inputs.sha || github.sha }}

jobs:
  propose:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      copilot-requests: write
    steps:
      - name: Checkout
        uses: actions/checkout@v4
        with:
          ref: ${{ env.MERGE_SHA }}
          fetch-depth: 0
          persist-credentials: false

      - name: Retrieve Node.js version
        id: node-version
        run: echo "node-version=$(grep '^nodejs ' .tool-versions | awk '{print $2}')" >> "$GITHUB_OUTPUT"

      - name: Setup Node.js
        uses: actions/setup-node@v4
        with:
          node-version: ${{ steps.node-version.outputs.node-version }}
          cache: 'npm'
          cache-dependency-path: docs/package-lock.json

      - name: Install Copilot CLI
        run: npm install -g @github/copilot@1.0.90

      - name: Propose documentation edits
        env:
          GITHUB_TOKEN: ${{ github.token }}
        run: scripts/docs-proposal/propose.sh "$MERGE_SHA" .docs-proposal

      - name: Upload proposal
        uses: actions/upload-artifact@v4
        with:
          name: docs-proposal
          path: .docs-proposal/
          include-hidden-files: true
          if-no-files-found: error

  publish:
    needs: propose
    runs-on: ubuntu-latest
    permissions:
      contents: write
      pull-requests: write
    steps:
      - name: Checkout
        uses: actions/checkout@v4
        with:
          ref: ${{ env.MERGE_SHA }}
          fetch-depth: 0

      - name: Retrieve Node.js version
        id: node-version
        run: echo "node-version=$(grep '^nodejs ' .tool-versions | awk '{print $2}')" >> "$GITHUB_OUTPUT"

      - name: Setup Node.js
        uses: actions/setup-node@v4
        with:
          node-version: ${{ steps.node-version.outputs.node-version }}
          cache: 'npm'
          cache-dependency-path: docs/package-lock.json

      - name: Download proposal
        uses: actions/download-artifact@v4
        with:
          name: docs-proposal
          path: .docs-proposal

      - name: Publish proposal pull request
        env:
          GH_TOKEN: ${{ github.token }}
        run: scripts/docs-proposal/publish.sh "$MERGE_SHA" .docs-proposal
```

- [ ] **Step 2: Add the CI job to `format.yml`** (after `format-docs`)

```yaml
docs-proposal-scripts:
  name: docs-proposal scripts
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4

    - name: Retrieve Node.js version
      id: node-version
      run: echo "node-version=$(grep '^nodejs ' .tool-versions | awk '{print $2}')" >> $GITHUB_OUTPUT
      shell: bash

    - name: Use Node.js
      uses: actions/setup-node@v4
      with:
        node-version: ${{ steps.node-version.outputs.node-version }}

    - name: Run shellcheck
      run: shellcheck scripts/docs-proposal/*.sh

    - name: Test hunk review builder
      run: node --test scripts/docs-proposal/hunks.test.mjs

    - name: Test proposal scripts
      run: scripts/docs-proposal/test.sh
```

- [ ] **Step 3: Document in `scripts/README.md`** — add table rows:

```markdown
| `docs-proposal/propose.sh` | Merge SHA on `main`, work dir; Copilot CLI, Node, `npm` | Runs Copilot CLI to edit `docs/guide/**` or `docs/index.md`, formats docs, and writes a patch, rationale, and evidence into the work dir. Leaves the docs edits staged. |
| `docs-proposal/publish.sh` | Merge SHA, work dir from `propose.sh`; `gh` with write `GH_TOKEN` | Applies the patch, runs docs gates, force-pushes `docs/auto/<sha12>`, opens or edits its PR, and posts a per-hunk review. Never writes `main`. |
| `docs-proposal/test.sh` | Git, Node | Tests the helpers and publish flow against a temporary repository with stubbed `gh` and `npm`; removes it on exit. |
```

Then append after the table:

```markdown
The `Documentation proposal` workflow runs both proposal scripts after code
merges to `main`. It needs the organization Copilot policy "Allow use of
Copilot CLI billed to the organization" and the repository setting "Allow
GitHub Actions to create and approve pull requests". Pull requests opened with
`GITHUB_TOKEN` do not trigger other workflows, so the publish job runs the docs
gates itself; push to the proposal branch to run regular CI. Retry a merge with
the workflow's manual dispatch and its full commit SHA.
```

Run `cd docs && npx prettier --write ../scripts/README.md` only if the docs
Prettier config covers it; otherwise align the table columns by hand.

- [ ] **Step 4: Correct the spec** in
      `docs/superpowers/specs/2026-10-01-docs-proposal-automation-design.md`:

- Per-hunk table, last row: anchor "Listed in the review body", suggestion "None".
- Trigger: add that `scripts/docs-proposal/**` is excluded and that dispatch
  requires a full SHA reachable from `origin/main`.
- `publish` step 1: a closed or merged proposal for the SHA is left alone.
- Testing: CI runs the scripts' tests in a new `format.yml` job.

Run: `cd docs && npx prettier --write superpowers/specs/2026-10-01-docs-proposal-automation-design.md superpowers/plans/2026-10-01-docs-proposal-automation.md`

- [ ] **Step 5: Validate and commit**

Run: `actionlint .github/workflows/docs-proposal.yml .github/workflows/format.yml` if
installed, else `python3 -c "import yaml,sys; [yaml.safe_load(open(f)) for f in sys.argv[1:]]" .github/workflows/docs-proposal.yml .github/workflows/format.yml`.
Expected: no errors.

```bash
git add .github/workflows/docs-proposal.yml .github/workflows/format.yml scripts/README.md docs/superpowers
git commit -S --signoff -m "Add documentation proposal workflow" -m "Closes #1105."
```

---

### Task 6: Verification

- [ ] `node --test scripts/docs-proposal/hunks.test.mjs` — all pass.
- [ ] `scripts/docs-proposal/test.sh` — all pass.
- [ ] `cd docs && npm run lint && npm run format && npm run build` — pass.
- [ ] `cargo fmt --all -- --check` — unaffected, pass.
- [ ] After merge: dispatch `Documentation proposal` with a recent merge SHA
      that changed a CLI flag; confirm a `docs/auto/<sha12>` PR with per-hunk
      review comments, and that a second dispatch with the same SHA does not
      re-post.
