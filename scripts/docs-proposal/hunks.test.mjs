import assert from "node:assert/strict";
import { test } from "node:test";

import {
  buildReview,
  collectHunks,
  indexEvidence,
  listHunks,
  parseDiff,
  revertSuggestion,
} from "./hunks.mjs";

const MODIFIED = [
  "diff --git a/docs/guide/cli.md b/docs/guide/cli.md",
  "index 1111111..2222222 100644",
  "--- a/docs/guide/cli.md",
  "+++ b/docs/guide/cli.md",
  "@@ -10,4 +10,4 @@ heading",
  " keep one",
  "-old flag",
  "+new flag",
  " keep two",
  "",
].join("\n");

test("parseDiff numbers context, removed, and added lines", () => {
  const [file] = parseDiff(MODIFIED);
  assert.equal(file.path, "docs/guide/cli.md", "should take the b/ path");
  assert.equal(file.status, "modified", "should default to modified");
  assert.deepEqual(
    file.hunks[0].lines,
    [
      { type: " ", text: "keep one", oldNo: 10, newNo: 10 },
      { type: "-", text: "old flag", oldNo: 11, newNo: null },
      { type: "+", text: "new flag", oldNo: null, newNo: 11 },
      { type: " ", text: "keep two", oldNo: 12, newNo: 12 },
    ],
    "should track old and new line numbers",
  );
});

test("parseDiff records added, deleted, and binary files", () => {
  const diff = [
    "diff --git a/docs/guide/new.md b/docs/guide/new.md",
    "new file mode 100644",
    "--- /dev/null",
    "+++ b/docs/guide/new.md",
    "@@ -0,0 +1 @@",
    "+hello",
    "diff --git a/docs/guide/gone.md b/docs/guide/gone.md",
    "deleted file mode 100644",
    "--- a/docs/guide/gone.md",
    "+++ /dev/null",
    "@@ -1 +0,0 @@",
    "-bye",
    "diff --git a/docs/guide/logo.png b/docs/guide/logo.png",
    "Binary files a/docs/guide/logo.png and b/docs/guide/logo.png differ",
    "",
  ].join("\n");
  const files = parseDiff(diff);
  assert.deepEqual(
    files.map(({ path, status, binary }) => ({ path, status, binary })),
    [
      { path: "docs/guide/new.md", status: "added", binary: false },
      { path: "docs/guide/gone.md", status: "deleted", binary: false },
      { path: "docs/guide/logo.png", status: "modified", binary: true },
    ],
    "should classify each file",
  );
  assert.deepEqual(
    files[0].hunks[0].lines,
    [{ type: "+", text: "hello", oldNo: null, newNo: 1 }],
    "should start a new file at line 1",
  );
});

test("parseDiff skips no-newline markers", () => {
  const diff = MODIFIED.replace(
    " keep two\n",
    " keep two\n\\ No newline at end of file\n",
  );
  const [file] = parseDiff(diff);
  assert.equal(file.hunks[0].lines.length, 4, "should ignore the marker line");
});

test("collectHunks gives stable ids and skips binary files", () => {
  const first = collectHunks(parseDiff(MODIFIED));
  const second = collectHunks(
    parseDiff(MODIFIED.replace("@@ -10,4 +10,4", "@@ -20,4 +20,4")),
  );
  assert.equal(first.length, 1, "should collect the single hunk");
  assert.match(
    first[0].id,
    /^docs\/guide\/cli\.md#[0-9a-f]{8}$/,
    "should use path#hash",
  );
  assert.equal(first[0].id, second[0].id, "should not depend on line numbers");
});

test("collectHunks suffixes duplicate ids", () => {
  const twice = MODIFIED + MODIFIED.split("\n").slice(4).join("\n");
  const ids = collectHunks(parseDiff(twice)).map((entry) => entry.id);
  assert.equal(ids.length, 2, "should collect both hunks");
  assert.equal(ids[1], `${ids[0]}-2`, "should suffix the repeated id");
});

test("listHunks exposes removed and added text", () => {
  const [entry] = listHunks(parseDiff(MODIFIED));
  assert.deepEqual(entry.removed, ["old flag"], "should list removed lines");
  assert.deepEqual(entry.added, ["new flag"], "should list added lines");
  assert.equal(entry.status, "modified", "should carry the file status");
});

function hunkOf(lines, oldStart = 1, newStart = 1) {
  const diff = [
    "diff --git a/docs/guide/a.md b/docs/guide/a.md",
    "--- a/docs/guide/a.md",
    "+++ b/docs/guide/a.md",
    `@@ -${oldStart} +${newStart} @@`,
    ...lines,
    "",
  ].join("\n");
  return parseDiff(diff)[0].hunks[0];
}

test("revertSuggestion restores replaced lines", () => {
  const suggestion = revertSuggestion(hunkOf([" a", "-b", "+B", "+B2", " c"]));
  assert.deepEqual(
    suggestion,
    { startLine: 2, line: 3, body: ["b"] },
    "should span the new lines and restore the old one",
  );
});

test("revertSuggestion deletes pure insertions", () => {
  const suggestion = revertSuggestion(hunkOf([" a", "+new", " c"]));
  assert.deepEqual(
    suggestion,
    { startLine: 2, line: 2, body: [] },
    "should suggest an empty body",
  );
});

test("revertSuggestion spans several change groups with their context", () => {
  const suggestion = revertSuggestion(hunkOf(["-a", "+A", " mid", "-c", "+C"]));
  assert.deepEqual(
    suggestion,
    { startLine: 1, line: 3, body: ["a", "mid", "c"] },
    "should keep the middle context line",
  );
});

test("revertSuggestion anchors pure deletions on the previous context line", () => {
  const suggestion = revertSuggestion(hunkOf([" a", "-gone", " c"], 4, 4));
  assert.deepEqual(
    suggestion,
    { startLine: 4, line: 4, body: ["a", "gone"] },
    "should restore the deleted line after its anchor",
  );
});

test("revertSuggestion anchors leading deletions on the next context line", () => {
  const suggestion = revertSuggestion(hunkOf(["-gone", " a"]));
  assert.deepEqual(
    suggestion,
    { startLine: 1, line: 1, body: ["gone", "a"] },
    "should restore the deleted line before its anchor",
  );
});

test("revertSuggestion returns null without an anchor", () => {
  assert.equal(
    revertSuggestion(hunkOf(["-only"])),
    null,
    "should not invent an anchor",
  );
});

test("indexEvidence keeps valid entries and sanitizes text", () => {
  const evidence = indexEvidence({
    hunks: [
      {
        id: "x#1",
        source: "crates/a.rs:4",
        reason: "Adds `--flag`\nfor @team",
      },
      { id: 7, source: "ignored" },
      { id: "x#2" },
      "junk",
    ],
  });
  assert.deepEqual(
    [...evidence],
    [["x#1", { source: "crates/a.rs:4", reason: "Adds --flag for @​team" }]],
    "should drop invalid entries and strip backticks, newlines, and mentions",
  );
  assert.equal(indexEvidence(null).size, 0, "should accept missing evidence");
});

test("buildReview cites evidence and suggests reverts", () => {
  const files = parseDiff(
    [
      "diff --git a/docs/guide/a.md b/docs/guide/a.md",
      "--- a/docs/guide/a.md",
      "+++ b/docs/guide/a.md",
      "@@ -1,3 +1,3 @@",
      " a",
      "-uses ```fence```",
      "+B",
      " c",
      "@@ -10 +10,2 @@",
      " j",
      "+k",
      "",
    ].join("\n"),
  );
  const [first] = collectHunks(files);
  const evidence = indexEvidence({
    hunks: [
      { id: first.id, source: "crates/a.rs:9", reason: "Renames the flag." },
    ],
  });
  const review = buildReview(files, evidence, "abc123");

  assert.equal(review.commit_id, "abc123", "should pin the reviewed commit");
  assert.equal(
    review.event,
    "COMMENT",
    "should not approve or request changes",
  );
  assert.equal(review.comments.length, 2, "should comment on every hunk");
  assert.deepEqual(
    review.comments[0],
    {
      path: "docs/guide/a.md",
      line: 2,
      side: "RIGHT",
      body: [
        "**Source:** `crates/a.rs:9` — Renames the flag.",
        "",
        "Apply this suggestion to revert the hunk.",
        "",
        "````suggestion",
        "uses ```fence```",
        "````",
      ].join("\n"),
    },
    "should escalate the fence and omit start_line for one line",
  );
  assert.match(
    review.comments[1].body,
    /none cited/,
    "should flag hunks without evidence",
  );
  assert.match(
    review.comments[1].body,
    /```suggestion\n```$/,
    "should suggest deleting an insertion",
  );
});

test("buildReview lists binary and unanchored changes in the body", () => {
  const files = parseDiff(
    [
      "diff --git a/docs/guide/gone.md b/docs/guide/gone.md",
      "deleted file mode 100644",
      "@@ -1 +0,0 @@",
      "-bye",
      "diff --git a/docs/guide/logo.png b/docs/guide/logo.png",
      "Binary files a/docs/guide/logo.png and b/docs/guide/logo.png differ",
      "",
    ].join("\n"),
  );
  const review = buildReview(files, new Map(), "abc123");
  assert.equal(review.comments.length, 0, "should not post line comments");
  assert.match(
    review.body,
    /`docs\/guide\/gone\.md`: file deleted/,
    "should note the deletion",
  );
  assert.match(
    review.body,
    /`docs\/guide\/logo\.png`: binary change/,
    "should note the binary file",
  );
});

test("buildReview adds start_line for multi-line spans", () => {
  const files = parseDiff(
    [
      "diff --git a/docs/index.md b/docs/index.md",
      "@@ -1 +1,2 @@",
      "-a",
      "+b",
      "+c",
      "",
    ].join("\n"),
  );
  const [comment] = buildReview(files, new Map(), "abc123").comments;
  assert.equal(comment.start_line, 1, "should start at the first new line");
  assert.equal(
    comment.start_side,
    "RIGHT",
    "should anchor the start on the new side",
  );
  assert.equal(comment.line, 2, "should end at the last new line");
});
