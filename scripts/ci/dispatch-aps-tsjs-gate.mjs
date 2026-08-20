#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { existsSync } from "node:fs";
import process from "node:process";
import { fileURLToPath } from "node:url";

const SHA_PATTERN = /^[0-9a-f]{40}$/u;
const RELEASE_PATTERN = /^[0-9a-f]{64}$/u;
const TOKEN_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/u;
const REF_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._/-]{0,127}$/u;
const RUN_LIST_FIELDS = "databaseId,displayTitle,headSha,createdAt";
const WORKFLOWS = Object.freeze({
  performance: ".github/workflows/tsjs-performance-gate.yml",
  "real-gam": ".github/workflows/aps-real-gam.yml",
});

function fail(message) {
  throw new Error(`APS/TSJS dispatch refused: ${message}`);
}

function exactSha(value, label) {
  if (!SHA_PATTERN.test(value ?? ""))
    fail(`${label} must be 40 lowercase hex characters`);
  return value;
}

function exactToken(value, label) {
  if (!TOKEN_PATTERN.test(value ?? "")) fail(`${label} is invalid`);
  return value;
}

function exactRef(value) {
  if (
    !REF_PATTERN.test(value ?? "") ||
    value.includes("..") ||
    value.includes("//") ||
    value.endsWith("/") ||
    value.endsWith(".lock")
  ) {
    fail("ref is invalid");
  }
  return value;
}

function defaultRunner(command, args, options = {}) {
  return execFileSync(command, args, {
    cwd: process.cwd(),
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
    ...options,
  });
}

function defaultSleep(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

export function createEvidenceId({ kind, mode, headSha, now, randomSuffix }) {
  if (kind !== "performance" && kind !== "real-gam")
    fail("gate kind is invalid");
  exactSha(headSha, "head SHA");
  if (!(now instanceof Date) || !Number.isFinite(now.valueOf()))
    fail("dispatch time is invalid");
  if (!/^[0-9a-f]{8}$/u.test(randomSuffix ?? ""))
    fail("random suffix is invalid");
  const timestamp = now
    .toISOString()
    .replace(/[-:]/gu, "")
    .replace(/\.\d{3}Z$/u, "Z");
  const phase = kind === "performance" ? exactToken(mode, "mode") : "cutover";
  const evidenceId = `aps-tsjs-${kind}-${phase}-${headSha.slice(0, 12)}-${timestamp}-${randomSuffix}`;
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]{7,127}$/u.test(evidenceId)) {
    fail("generated evidence id is invalid");
  }
  return evidenceId;
}

function workflowRun(value) {
  if (typeof value !== "object" || value === null || Array.isArray(value))
    return undefined;
  if (
    !Number.isSafeInteger(value.databaseId) ||
    value.databaseId < 1 ||
    typeof value.displayTitle !== "string" ||
    typeof value.headSha !== "string" ||
    typeof value.createdAt !== "string"
  ) {
    return undefined;
  }
  return value;
}

export function selectUniqueWorkflowRun(runs, expected) {
  if (!Array.isArray(runs)) fail("GitHub run list must be an array");
  const matches = runs
    .map(workflowRun)
    .filter(
      (run) =>
        run !== undefined &&
        run.headSha === expected.headSha &&
        run.displayTitle === expected.displayTitle,
    );
  if (matches.length > 1)
    fail("multiple workflow runs match the exact dispatch");
  const match = matches[0];
  if (!match) return undefined;
  const createdAt = new Date(match.createdAt);
  if (
    !Number.isFinite(createdAt.valueOf()) ||
    createdAt.valueOf() < expected.dispatchStartedAt.valueOf()
  ) {
    fail("stale workflow run predates the dispatch");
  }
  return match;
}

async function runCommand(runner, command, args, options) {
  const output = await runner(command, args, options);
  if (typeof output !== "string") fail(`${command} returned non-text output`);
  return output;
}

async function resolveLocalAndRemoteHead(ref, runner) {
  const status = await runCommand(runner, "git", [
    "status",
    "--porcelain=v1",
    "--untracked-files=all",
  ]);
  if (status !== "") fail("worktree must be clean before dispatch");
  const headSha = exactSha(
    (await runCommand(runner, "git", ["rev-parse", "HEAD"])).trim(),
    "local HEAD",
  );
  const remote = (
    await runCommand(runner, "git", [
      "ls-remote",
      "--heads",
      "origin",
      `refs/heads/${ref}`,
    ])
  ).trim();
  const rows = remote === "" ? [] : remote.split("\n");
  if (rows.length !== 1) fail("remote branch must resolve exactly once");
  const [remoteSha, remoteRef, ...extra] = rows[0].split(/\s+/u);
  if (
    extra.length !== 0 ||
    remoteRef !== `refs/heads/${ref}` ||
    remoteSha !== headSha
  ) {
    fail("remote branch must resolve to local HEAD");
  }
  return headSha;
}

async function waitForWorkflowRun({
  workflow,
  ref,
  evidenceId,
  headSha,
  displayTitle,
  dispatchStartedAt,
  runner,
  sleep,
}) {
  for (let attempt = 0; attempt < 30; attempt += 1) {
    const raw = await runCommand(runner, "gh", [
      "run",
      "list",
      "--workflow",
      workflow,
      "--branch",
      ref,
      "--event",
      "workflow_dispatch",
      "--limit",
      "100",
      "--json",
      RUN_LIST_FIELDS,
    ]);
    let runs;
    try {
      runs = JSON.parse(raw);
    } catch {
      fail("GitHub run list returned invalid JSON");
    }
    const match = selectUniqueWorkflowRun(runs, {
      evidenceId,
      headSha,
      displayTitle,
      dispatchStartedAt,
    });
    if (match) return match;
    if (attempt < 29) await sleep(2_000);
  }
  fail("zero workflow runs matched the exact dispatch");
}

export async function dispatchGate(input, dependencies = {}) {
  const runner = dependencies.runner ?? defaultRunner;
  const now = dependencies.now ?? (() => new Date());
  const randomSuffix =
    dependencies.randomSuffix ?? (() => randomBytes(4).toString("hex"));
  const sleep = dependencies.sleep ?? defaultSleep;
  const pathExists = dependencies.pathExists ?? existsSync;
  const kind = input.kind;
  const workflow = WORKFLOWS[kind];
  if (!workflow) fail("gate kind is invalid");
  const ref = exactRef(input.ref);
  const outputDir = input.outputDir;
  if (typeof outputDir !== "string" || outputDir.length === 0)
    fail("output directory is required");
  if (pathExists(outputDir)) fail("output directory must not already exist");

  let mode;
  let baseSha;
  let previousArtifactId;
  if (kind === "performance") {
    mode = input.mode;
    if (mode !== "preswitch" && mode !== "postswitch")
      fail("performance mode is invalid");
    baseSha = exactSha(input.baseSha, "base SHA");
  } else {
    previousArtifactId = exactToken(
      input.previousArtifactId,
      "previous artifact id",
    );
  }

  const headSha = await resolveLocalAndRemoteHead(ref, runner);
  let releaseId;
  if (kind === "real-gam") {
    releaseId = (
      await runCommand(runner, "npm", [
        "--prefix",
        "crates/trusted-server-js/lib",
        "run",
        "--silent",
        "print:release-id",
      ])
    ).trim();
    if (!RELEASE_PATTERN.test(releaseId))
      fail("generated release id is invalid");
  }

  const observedDispatchTime = now();
  if (
    !(observedDispatchTime instanceof Date) ||
    !Number.isFinite(observedDispatchTime.valueOf())
  ) {
    fail("dispatch time is invalid");
  }
  const dispatchStartedAt = new Date(
    Math.floor(observedDispatchTime.valueOf() / 1_000) * 1_000,
  );
  const evidenceId = createEvidenceId({
    kind,
    mode,
    headSha,
    now: dispatchStartedAt,
    randomSuffix: randomSuffix(),
  });
  const displayTitle =
    kind === "performance"
      ? `TSJS Performance Gate / ${evidenceId} / ${mode}`
      : `APS real-GAM / ${evidenceId} / ${releaseId}`;
  const fields =
    kind === "performance"
      ? [
          "-f",
          `evidence_id=${evidenceId}`,
          "-f",
          `mode=${mode}`,
          "-f",
          `base_sha=${baseSha}`,
        ]
      : [
          "-f",
          `evidence_id=${evidenceId}`,
          "-f",
          `release_id=${releaseId}`,
          "-f",
          `previous_artifact_id=${previousArtifactId}`,
        ];
  await runCommand(runner, "gh", [
    "workflow",
    "run",
    workflow,
    "--ref",
    ref,
    ...fields,
  ]);
  const run = await waitForWorkflowRun({
    workflow,
    ref,
    evidenceId,
    headSha,
    displayTitle,
    dispatchStartedAt,
    runner,
    sleep,
  });
  const runId = run.databaseId;
  await runCommand(runner, "gh", [
    "run",
    "watch",
    String(runId),
    "--exit-status",
  ]);
  const artifactName =
    kind === "performance"
      ? `tsjs-performance-${evidenceId}`
      : `aps-real-gam-${runId}`;
  await runCommand(runner, "gh", [
    "run",
    "download",
    String(runId),
    "--name",
    artifactName,
    "--dir",
    outputDir,
  ]);

  if (kind === "performance") {
    await runCommand(runner, "node", [
      "scripts/validate-tsjs-performance-evidence.mjs",
      "--file",
      `${outputDir}/tsjs-performance-${mode}.json`,
      "--evidence-id",
      evidenceId,
      "--head-sha",
      headSha,
      "--base-sha",
      baseSha,
      "--mode",
      mode,
    ]);
  } else {
    await runCommand(runner, "node", [
      "scripts/ci/aps-tsjs-evidence.mjs",
      "validate-real-gam",
      outputDir,
      evidenceId,
      releaseId,
      headSha,
      String(runId),
      previousArtifactId,
    ]);
  }
  return { evidenceId, headSha, runId };
}

function parseCli(argv) {
  const kind = argv[0];
  if (kind !== "performance" && kind !== "real-gam")
    fail("first argument must be performance or real-gam");
  const values = new Map();
  for (let index = 1; index < argv.length; index += 2) {
    const name = argv[index];
    const value = argv[index + 1];
    if (!name?.startsWith("--") || value === undefined || values.has(name))
      fail("invalid CLI arguments");
    values.set(name, value);
  }
  const allowed =
    kind === "performance"
      ? new Set(["--ref", "--base-sha", "--mode", "--output-dir"])
      : new Set(["--ref", "--previous-artifact-id", "--output-dir"]);
  if (
    values.size !== allowed.size ||
    [...values.keys()].some((name) => !allowed.has(name))
  ) {
    fail("missing or unknown CLI arguments");
  }
  return kind === "performance"
    ? {
        kind,
        ref: values.get("--ref"),
        baseSha: values.get("--base-sha"),
        mode: values.get("--mode"),
        outputDir: values.get("--output-dir"),
      }
    : {
        kind,
        ref: values.get("--ref"),
        previousArtifactId: values.get("--previous-artifact-id"),
        outputDir: values.get("--output-dir"),
      };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  try {
    const result = await dispatchGate(parseCli(process.argv.slice(2)));
    process.stdout.write(`${JSON.stringify(result)}\n`);
  } catch (error) {
    process.stderr.write(
      `${error instanceof Error ? error.message : String(error)}\n`,
    );
    process.exitCode = 1;
  }
}
