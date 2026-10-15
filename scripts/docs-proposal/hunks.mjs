// Parses a unified documentation diff into hunks and builds the per-hunk
// review that cites Copilot evidence and offers a suggestion reverting each
// hunk. Dependency-free so it runs on any runner with Node.
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const FILE_HEADER = /^diff --git a\/(.+) b\/(.+)$/;
const HUNK_HEADER = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

export function parseDiff(text) {
  const files = [];
  let file = null;
  let hunk = null;
  let oldNo = 0;
  let newNo = 0;
  for (const raw of text.split("\n")) {
    const fileHeader = FILE_HEADER.exec(raw);
    if (fileHeader) {
      file = {
        path: fileHeader[2],
        status: "modified",
        binary: false,
        hunks: [],
      };
      files.push(file);
      hunk = null;
      continue;
    }
    if (!file) {
      continue;
    }
    const hunkHeader = HUNK_HEADER.exec(raw);
    if (hunkHeader) {
      oldNo = Number(hunkHeader[1]);
      newNo = Number(hunkHeader[2]);
      hunk = { lines: [] };
      file.hunks.push(hunk);
      continue;
    }
    if (!hunk) {
      if (raw.startsWith("new file mode")) {
        file.status = "added";
      } else if (raw.startsWith("deleted file mode")) {
        file.status = "deleted";
      } else if (raw.startsWith("Binary files ")) {
        file.binary = true;
      }
      continue;
    }
    const type = raw[0];
    const lineText = raw.slice(1);
    if (type === " ") {
      hunk.lines.push({ type, text: lineText, oldNo: oldNo++, newNo: newNo++ });
    } else if (type === "-") {
      hunk.lines.push({ type, text: lineText, oldNo: oldNo++, newNo: null });
    } else if (type === "+") {
      hunk.lines.push({ type, text: lineText, oldNo: null, newNo: newNo++ });
    }
  }
  return files;
}

function hashHunk(path, hunk) {
  const hash = createHash("sha256");
  hash.update(path);
  for (const line of hunk.lines) {
    if (line.type !== " ") {
      hash.update(`\n${line.type}${line.text}`);
    }
  }
  return `${path}#${hash.digest("hex").slice(0, 8)}`;
}

export function collectHunks(files) {
  const seen = new Map();
  const entries = [];
  for (const file of files) {
    if (file.binary) {
      continue;
    }
    for (const hunk of file.hunks) {
      const base = hashHunk(file.path, hunk);
      const count = (seen.get(base) ?? 0) + 1;
      seen.set(base, count);
      const id = count === 1 ? base : `${base}-${count}`;
      entries.push({ id, path: file.path, status: file.status, hunk });
    }
  }
  return entries;
}

export function listHunks(files) {
  return collectHunks(files).map(({ id, path, status, hunk }) => ({
    id,
    path,
    status,
    removed: hunk.lines
      .filter((line) => line.type === "-")
      .map((line) => line.text),
    added: hunk.lines
      .filter((line) => line.type === "+")
      .map((line) => line.text),
  }));
}

// Returns the comment anchor and the original text that undoes a hunk, or
// null when the hunk has no new-side line to anchor a suggestion on.
export function revertSuggestion(hunk) {
  const { lines } = hunk;
  const first = lines.findIndex((line) => line.type !== " ");
  const last = lines.findLastIndex((line) => line.type !== " ");
  if (first === -1) {
    return null;
  }
  const span = lines.slice(first, last + 1);
  const newSide = span.filter((line) => line.type !== "-");
  const oldText = span
    .filter((line) => line.type !== "+")
    .map((line) => line.text);
  if (newSide.length > 0) {
    return {
      startLine: newSide[0].newNo,
      line: newSide.at(-1).newNo,
      body: oldText,
    };
  }
  const before = lines[first - 1];
  if (before?.type === " ") {
    return {
      startLine: before.newNo,
      line: before.newNo,
      body: [before.text, ...oldText],
    };
  }
  const after = lines[last + 1];
  if (after?.type === " ") {
    return {
      startLine: after.newNo,
      line: after.newNo,
      body: [...oldText, after.text],
    };
  }
  return null;
}

const MAX_EVIDENCE_LENGTH = 300;

// Model output lands in pull request bodies and review comments, so a zero
// width space after every `@` keeps it from pinging a user or team.
export function neutralizeMentions(text) {
  return text.replace(/@/g, "@​");
}

// Evidence also lands in an inline code span, so strip anything that could
// break out of it.
function sanitize(value) {
  if (typeof value !== "string") {
    return "";
  }
  return neutralizeMentions(value.replace(/[`\r\n]+/g, " "))
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, MAX_EVIDENCE_LENGTH);
}

export function indexEvidence(raw) {
  const evidence = new Map();
  const items = Array.isArray(raw?.hunks) ? raw.hunks : [];
  for (const item of items) {
    if (typeof item?.id !== "string") {
      continue;
    }
    const source = sanitize(item.source);
    const reason = sanitize(item.reason);
    if (source || reason) {
      evidence.set(item.id, { source, reason });
    }
  }
  return evidence;
}

// A fence must be longer than any backtick run in its body.
function fenceFor(body) {
  let longestRun = 0;
  for (const text of body) {
    for (const run of text.match(/`+/g) ?? []) {
      longestRun = Math.max(longestRun, run.length);
    }
  }
  return "`".repeat(Math.max(3, longestRun + 1));
}

// The rationale is model output, so render it as a plain text block: links,
// images, HTML, issue references, and closing keywords in it stay inert.
export function inertBlock(text) {
  const body = neutralizeMentions(text).replace(/\n+$/, "");
  const fence = fenceFor(body.split("\n"));
  return `${fence}text\n${body}\n${fence}\n`;
}

// A null body marks a hunk that creates its file: an empty suggestion would
// leave an empty page that VitePress still builds, so ask for a deletion.
function commentBody(cited, body) {
  const source = cited?.source
    ? `\`${cited.source}\``
    : "none cited — verify this hunk against the merged code.";
  const reason = cited?.reason ? ` — ${cited.reason}` : "";
  const heading = `**Source:** ${source}${reason}`;
  if (body === null) {
    return [
      heading,
      "",
      "This hunk creates the file. Delete the file to revert it.",
    ].join("\n");
  }
  const fence = fenceFor(body);
  const content = body.length > 0 ? `${body.join("\n")}\n` : "";
  return [
    heading,
    "",
    "Apply this suggestion to revert the hunk.",
    "",
    `${fence}suggestion\n${content}${fence}`,
  ].join("\n");
}

const REVIEW_INTRO =
  "Copilot evidence for each proposed hunk. Leave a hunk to accept it, or apply its suggestion to revert it. Verify every cited source before merging.";

export function buildReview(files, evidence, commitId) {
  const comments = [];
  const notes = [];
  for (const file of files) {
    if (file.binary) {
      notes.push(
        `- \`${file.path}\`: binary change — review the file directly.`,
      );
    }
  }
  for (const { id, path, status, hunk } of collectHunks(files)) {
    const suggestion = revertSuggestion(hunk);
    if (!suggestion) {
      const reason =
        status === "deleted" ? "file deleted" : "no line to anchor a revert";
      notes.push(`- \`${path}\`: ${reason} — revert it manually if needed.`);
      continue;
    }
    const comment = {
      path,
      line: suggestion.line,
      side: "RIGHT",
      body: commentBody(
        evidence.get(id),
        status === "added" ? null : suggestion.body,
      ),
    };
    if (suggestion.startLine !== suggestion.line) {
      comment.start_line = suggestion.startLine;
      comment.start_side = "RIGHT";
    }
    comments.push(comment);
  }
  const body =
    notes.length > 0
      ? `${REVIEW_INTRO}\n\nChanges without a suggestion:\n\n${notes.join("\n")}`
      : REVIEW_INTRO;
  return { commit_id: commitId, event: "COMMENT", body, comments };
}

function readEvidence(path) {
  try {
    return indexEvidence(JSON.parse(readFileSync(path, "utf8")));
  } catch (error) {
    process.stderr.write(
      `Ignoring unreadable evidence at ${path}: ${error.message}\n`,
    );
    return new Map();
  }
}

function main([command, evidencePath, commitId]) {
  const input = readFileSync(0, "utf8");
  if (command === "rationale") {
    process.stdout.write(inertBlock(input));
    return;
  }
  const files = parseDiff(input);
  if (command === "list") {
    process.stdout.write(`${JSON.stringify(listHunks(files), null, 2)}\n`);
    return;
  }
  if (command === "review" && evidencePath && commitId) {
    const review = buildReview(files, readEvidence(evidencePath), commitId);
    process.stdout.write(`${JSON.stringify(review, null, 2)}\n`);
    return;
  }
  process.stderr.write(
    "usage: hunks.mjs list | review <evidence.json> <commit-sha> | rationale\n",
  );
  process.exitCode = 2;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2));
}
