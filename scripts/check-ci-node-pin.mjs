#!/usr/bin/env node
// Check that every CI job pins Node before it compiles Rust.
//
// trusted-server-core depends on trusted-server-js, whose build script runs the
// TSJS build with the `npm` on PATH. A job that compiles core before setting up
// the `.tool-versions` Node builds the embedded bundles with the runner image's
// Node instead (see #1204).
//
// For each job in `.github/workflows/*.yml`, the check walks the steps in order,
// inlining local composite actions (`uses: ./...`). It fails when a step that
// compiles Rust comes before an unconditional `actions/setup-node` step pinned
// to `.tool-versions`. A step compiles Rust when it runs `worker-build` or any
// `cargo` subcommand except `fmt`, `metadata`, `tree` and a registry `install`
// (so aliases, toolchain overrides and unknown subcommands count), directly or
// through a repository shell script. A compile step counts even when it has an
// `if:`, but a conditional setup-node step does not pin Node. A later setup-node
// step that installs another version, even conditionally, undoes an earlier
// pin; one that names no version changes nothing. A `node-version` taken from a
// step output counts as pinned, since the workflows read that output from
// `.tool-versions`.
//
// The check uses only the Node standard library and reads YAML by indentation,
// so it expects the block-style YAML the repository's workflows use. Run it
// with the `.tool-versions` Node: `node scripts/check-ci-node-pin.mjs`.

import {
  existsSync,
  readdirSync,
  readFileSync,
  realpathSync,
  statSync,
} from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseArgs } from "node:util";

const COMPILE = new RegExp(
  String.raw`\bcargo(?:(?:[ \t]|\\\n)+[+-]\S+)*(?:[ \t]|\\\n)+` +
    String.raw`(?!(?:fmt|metadata|tree|install(?![^\n]*--path))(?:\s|$))\w` +
    String.raw`|\bworker-build\b`,
);
const SCRIPT =
  /(?<![\w/.-])(?:\.\/)?((?:scripts|crates|\.github)\/[\w./-]+\.sh)\b/g;
const KEY = /^([\w.-]+):(?:\s+(.*))?$/;
const TRAILING_COMMENT = /\s+#.*$/;
const QUOTED = /^(?:'(?:[^']|'')*'|"(?:[^"\\]|\\.)*")/;
const STEP_OUTPUT = /^\$\{\{\s*steps\.[\w-]+\.outputs\.[\w-]+\s*\}\}$/;

/** Raised when a workflow or action does not have the expected shape. */
export class ParseError extends Error {}

function isFile(file) {
  return existsSync(file) && statSync(file).isFile();
}

function readLines(file) {
  const lines = readFileSync(file, "utf8").split(/\r\n|\r|\n/);
  if (lines.at(-1) === "") {
    lines.pop();
  }
  return lines;
}

function isContent(line) {
  const stripped = line.trim();
  return stripped !== "" && !stripped.startsWith("#");
}

function indentOf(line) {
  return line.length - line.replace(/^ +/, "").length;
}

function stripChars(value, chars) {
  let start = 0;
  let end = value.length;
  while (start < end && chars.includes(value[start])) {
    start += 1;
  }
  while (end > start && chars.includes(value[end - 1])) {
    end -= 1;
  }
  return value.slice(start, end);
}

function firstContent(lines, start, end) {
  for (let index = start; index < end; index += 1) {
    if (isContent(lines[index])) {
      return index;
    }
  }
  return null;
}

/**
 * Return the keys at the shallowest indent of `lines[start:end]` as a `Map` of
 * `{ line, value, start, end }` entries.
 */
export function mapping(lines, start, end) {
  const entries = new Map();
  const first = firstContent(lines, start, end);
  if (first === null) {
    return entries;
  }
  const base = indentOf(lines[first]);
  // A sequence may sit at its parent key's indent (`steps:` then `- run:`),
  // so its items belong to that key's block rather than ending it.
  const heads = [];
  for (let index = first; index < end; index += 1) {
    const line = lines[index];
    if (
      isContent(line) &&
      indentOf(line) <= base &&
      !line.trimStart().startsWith("-")
    ) {
      heads.push(index);
    }
  }
  heads.forEach((head, position) => {
    const match = KEY.exec(lines[head].trim());
    if (match === null) {
      return;
    }
    const stop = position + 1 < heads.length ? heads[position + 1] : end;
    let value = (match[2] ?? "").trim();
    // Keep a quoted scalar whole and drop any comment after it. A quote that
    // does not close on this line starts a multi-line scalar, which is kept
    // as is.
    const quoted = QUOTED.exec(value);
    if (quoted !== null) {
      value = quoted[0];
    } else if (!value.startsWith("'") && !value.startsWith('"')) {
      value = value.replace(TRAILING_COMMENT, "");
    }
    entries.set(match[1], { line: head, value, start: head + 1, end: stop });
  });
  return entries;
}

/** Return `[start, end]` for each item of the block sequence in range. */
export function sequence(lines, start, end) {
  const first = firstContent(lines, start, end);
  if (first === null) {
    return [];
  }
  const base = indentOf(lines[first]);
  const heads = [];
  for (let index = first; index < end; index += 1) {
    const line = lines[index];
    if (
      isContent(line) &&
      indentOf(line) === base &&
      line.trimStart().startsWith("-")
    ) {
      heads.push(index);
    }
  }
  return heads.map((head, position) => [
    head,
    position + 1 < heads.length ? heads[position + 1] : end,
  ]);
}

/** Return the inline value and block lines of an entry, without comments. */
function blockText(lines, entry) {
  return [
    entry.value,
    ...lines.slice(entry.start, entry.end).filter(isContent),
  ].join("\n");
}

function scriptPaths(text) {
  return Array.from(text.matchAll(SCRIPT), (match) => match[1]);
}

/** Return whether a repository script runs a compile command. */
function scriptCompiles(root, relative, seen) {
  const file = path.join(root, relative);
  if (seen.has(relative) || !isFile(file)) {
    return false;
  }
  seen.add(relative);
  const text = readLines(file).filter(isContent).join("\n");
  if (COMPILE.test(text)) {
    return true;
  }
  return scriptPaths(text).some((nested) => scriptCompiles(root, nested, seen));
}

/** Return whether a `run:` script compiles Rust, directly or through a repository script. */
export function runsCompile(root, text) {
  if (COMPILE.test(text)) {
    return true;
  }
  return scriptPaths(text).some((relative) =>
    scriptCompiles(root, relative, new Set()),
  );
}

function setupNodeOptions(lines, step) {
  const options = step.get("with");
  return options === undefined
    ? null
    : mapping(lines, options.start, options.end);
}

/** Return whether a setup-node step installs the `.tool-versions` Node. */
function pinsNode(lines, step) {
  const options = setupNodeOptions(lines, step);
  if (options === null) {
    return false;
  }
  // setup-node uses `node-version` and ignores the file when both are set.
  if (options.has("node-version")) {
    return STEP_OUTPUT.test(options.get("node-version").value);
  }
  if (options.has("node-version-file")) {
    let value = stripChars(options.get("node-version-file").value, "'\"");
    if (value.startsWith("./")) {
      value = value.slice(2);
    }
    return value === ".tool-versions";
  }
  return false;
}

/** Return whether a setup-node step names a Node version to install. */
function installsNode(lines, step) {
  const options = setupNodeOptions(lines, step);
  if (options === null) {
    return false;
  }
  return ["node-version", "node-version-file"].some(
    (key) => options.has(key) && options.get(key).value !== "",
  );
}

/** Yield the steps in range, inlining local composite actions. */
function* expandSteps(root, file, lines, start, end, conditional, active) {
  for (const [itemStart, itemEnd] of sequence(lines, start, end)) {
    // Read the item as a mapping by blanking out its `-` marker.
    const item = lines.slice();
    item[itemStart] = item[itemStart].replace("-", " ");
    const step = mapping(item, itemStart, itemEnd);
    const uses = step.has("uses")
      ? stripChars(step.get("uses").value, "'\"")
      : "";
    const stepConditional = conditional || step.has("if");
    if (uses.startsWith("./")) {
      let action = path.join(root, uses, "action.yml");
      if (!isFile(action)) {
        action = path.join(root, uses, "action.yaml");
      }
      if (!isFile(action)) {
        throw new ParseError(
          `${file}:${itemStart + 1}: no action.yml for \`${uses}\``,
        );
      }
      if (active.has(action)) {
        throw new ParseError(`${action}: composite action includes itself`);
      }
      yield* actionSteps(
        root,
        action,
        stepConditional,
        new Set([...active, action]),
      );
      continue;
    }
    const run = step.has("run") ? blockText(item, step.get("run")) : "";
    const setupNode = uses.startsWith("actions/setup-node@");
    yield {
      file,
      line: itemStart + 1,
      name: step.has("name")
        ? step.get("name").value
        : uses || run.trim().split("\n")[0],
      setsNode: setupNode && installsNode(item, step),
      pinsNode: setupNode && pinsNode(item, step),
      conditional: stepConditional,
      compiles: runsCompile(root, run),
    };
  }
}

function* actionSteps(root, file, conditional, active) {
  const lines = readLines(file);
  const top = mapping(lines, 0, lines.length);
  if (!top.has("runs")) {
    throw new ParseError(`${file}: no \`runs\` key`);
  }
  const runs = mapping(lines, top.get("runs").start, top.get("runs").end);
  if (!runs.has("steps")) {
    return;
  }
  const steps = runs.get("steps");
  yield* expandSteps(
    root,
    file,
    lines,
    steps.start,
    steps.end,
    conditional,
    active,
  );
}

/**
 * Check one workflow file.
 *
 * Returns the number of jobs checked and a violation message per bad job.
 * Throws a {@link ParseError} when the workflow does not have the expected shape.
 */
export function checkWorkflow(root, file) {
  const lines = readLines(file);
  const top = mapping(lines, 0, lines.length);
  if (!top.has("jobs")) {
    throw new ParseError(`${file}: no \`jobs\` key`);
  }
  const jobs = mapping(lines, top.get("jobs").start, top.get("jobs").end);
  if (jobs.size === 0) {
    throw new ParseError(`${file}: no jobs found`);
  }
  const violations = [];
  for (const [jobId, job] of jobs) {
    const keys = mapping(lines, job.start, job.end);
    if (!keys.has("steps")) {
      continue;
    }
    const steps = keys.get("steps");
    let pinned = false;
    for (const step of expandSteps(
      root,
      file,
      lines,
      steps.start,
      steps.end,
      false,
      new Set(),
    )) {
      if (step.setsNode) {
        // A conditional step that pins either runs and re-pins or is skipped.
        if (!step.pinsNode) {
          pinned = false;
        } else if (!step.conditional) {
          pinned = true;
        }
      } else if (step.compiles && !pinned) {
        violations.push(
          `${path.relative(root, step.file)}:${step.line}: job \`${jobId}\` in ` +
            `${path.relative(root, file)} compiles Rust in step \`${step.name}\` before ` +
            "an unconditional actions/setup-node step pinned to .tool-versions",
        );
        break;
      }
    }
  }
  return { jobCount: jobs.size, violations };
}

/** Return the workflow files under `root`, sorted by name. */
export function workflowFiles(root) {
  const directory = path.join(root, ".github", "workflows");
  if (!existsSync(directory)) {
    return [];
  }
  return readdirSync(directory)
    .filter((name) => /^.*\.y.*ml$/s.test(name))
    .sort()
    .map((name) => path.join(directory, name));
}

/**
 * Check every workflow under `root`.
 *
 * Returns the workflow and job counts and every violation found. Throws a
 * {@link ParseError} when a workflow or local action has an unexpected shape.
 */
export function checkRepository(root) {
  const workflows = workflowFiles(root);
  let jobCount = 0;
  const violations = [];
  for (const workflow of workflows) {
    const result = checkWorkflow(root, workflow);
    jobCount += result.jobCount;
    violations.push(...result.violations);
  }
  return { workflowCount: workflows.length, jobCount, violations };
}

function resolveRoot(root) {
  const resolved = path.resolve(root);
  return existsSync(resolved) ? realpathSync(resolved) : resolved;
}

/** Run the check from the command line and return the exit code. */
export function main(argv = process.argv.slice(2)) {
  let values;
  try {
    ({ values } = parseArgs({
      args: argv,
      options: {
        root: { type: "string" },
        help: { type: "boolean", short: "h" },
      },
    }));
  } catch (error) {
    console.error(`error: ${error.message}`);
    return 2;
  }
  if (values.help) {
    console.log(
      "usage: check-ci-node-pin.mjs [--root ROOT]\n\n" +
        "Check that every CI job pins Node before it compiles Rust.\n\n" +
        "  --root ROOT  repository root to check (default: this script's repository)",
    );
    return 0;
  }
  const root = resolveRoot(
    values.root ??
      path.join(path.dirname(fileURLToPath(import.meta.url)), ".."),
  );
  const workflowsDirectory = path.join(root, ".github", "workflows");
  if (workflowFiles(root).length === 0) {
    console.error(`no workflows found under ${workflowsDirectory}`);
    return 1;
  }
  let result;
  try {
    result = checkRepository(root);
  } catch (error) {
    if (error instanceof ParseError) {
      console.error(`error: ${error.message}`);
      return 1;
    }
    throw error;
  }
  for (const violation of result.violations) {
    console.error(violation);
  }
  if (result.violations.length > 0) {
    console.error(
      `${result.violations.length} job(s) compile Rust before pinning Node`,
    );
    return 1;
  }
  console.log(
    `ok: ${result.jobCount} jobs in ${result.workflowCount} workflows pin Node before compiling Rust`,
  );
  return 0;
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href
) {
  process.exitCode = main();
}
