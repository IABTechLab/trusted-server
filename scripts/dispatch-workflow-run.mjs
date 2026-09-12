#!/usr/bin/env node

import { execFileSync } from "node:child_process";

const POLL_INTERVAL_MS = 2_000;
const POLL_TIMEOUT_MS = 120_000;

function fail(message) {
  throw new Error(`[dispatch-workflow-run] ${message}`);
}

function run(command, args, options = {}) {
  try {
    return execFileSync(command, args, {
      cwd: options.cwd,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    }).trim();
  } catch (error) {
    const stderr =
      error && typeof error === "object" && "stderr" in error
        ? error.stderr
        : "";
    fail(
      `${command} ${args.join(" ")} failed${stderr ? `: ${String(stderr).trim()}` : ""}`,
    );
  }
}

function sleep(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function parseInputs(argumentsList) {
  const inputs = new Map();
  for (const argument of argumentsList) {
    const separator = argument.indexOf("=");
    if (separator <= 0)
      fail(`workflow input must use key=value syntax: ${argument}`);
    const key = argument.slice(0, separator);
    const value = argument.slice(separator + 1);
    if (!/^[A-Za-z_][A-Za-z0-9_-]*$/.test(key) || value.length === 0) {
      fail(`invalid workflow input: ${argument}`);
    }
    if (inputs.has(key)) fail(`duplicate workflow input: ${key}`);
    inputs.set(key, value);
  }
  return inputs;
}

function remoteShaForRef(ref) {
  const escapedRef = ref.replace(/^refs\/(heads|tags)\//, "");
  const output = run("git", [
    "ls-remote",
    "--heads",
    "--tags",
    "origin",
    `refs/heads/${escapedRef}`,
    `refs/tags/${escapedRef}`,
    `refs/tags/${escapedRef}^{}`,
  ]);
  const rows = output
    .split("\n")
    .filter(Boolean)
    .map((row) => row.split(/\s+/, 2));
  if (rows.length === 0) fail(`ref is not pushed to origin: ${ref}`);
  const dereferencedTag = rows.find(([, remoteRef]) =>
    remoteRef?.endsWith("^{}"),
  );
  return (dereferencedTag ?? rows[0])?.[0];
}

function listRuns(workflow) {
  const output = run("gh", [
    "run",
    "list",
    "--workflow",
    workflow,
    "--event",
    "workflow_dispatch",
    "--limit",
    "100",
    "--json",
    "databaseId,displayTitle,headBranch,headSha,createdAt",
  ]);
  try {
    return JSON.parse(output);
  } catch (error) {
    fail(
      `gh returned invalid run JSON: ${error instanceof Error ? error.message : error}`,
    );
  }
}

function matchingRuns(runs, evidenceId, sha, dispatchedAfter) {
  return runs.filter((candidate) => {
    const createdAt = Date.parse(candidate.createdAt);
    return (
      candidate.headSha === sha &&
      candidate.displayTitle?.includes(evidenceId) &&
      Number.isFinite(createdAt) &&
      createdAt >= dispatchedAfter
    );
  });
}

async function main() {
  const [workflow, ref, ...inputArguments] = process.argv.slice(2);
  if (!workflow || !ref) {
    fail(
      "usage: dispatch-workflow-run.mjs <workflow> <pushed-ref> key=value [...]",
    );
  }
  const inputs = parseInputs(inputArguments);
  const evidenceId = inputs.get("evidence_id");
  if (!evidenceId) fail("required workflow input is missing: evidence_id");
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]{7,127}$/.test(evidenceId)) {
    fail("evidence_id must be 8-128 URL-safe characters");
  }

  const localSha = run("git", ["rev-parse", `${ref}^{commit}`]);
  const remoteSha = remoteShaForRef(ref);
  if (localSha !== remoteSha) {
    fail(
      `ref is not pushed at the local commit: local ${localSha}, origin ${remoteSha}`,
    );
  }

  const existing = listRuns(workflow).filter((runRecord) =>
    runRecord.displayTitle?.includes(evidenceId),
  );
  if (existing.length > 0)
    fail(`evidence_id has already been used: ${evidenceId}`);

  const dispatchedAfter = Date.now() - 5_000;
  const dispatchArguments = ["workflow", "run", workflow, "--ref", ref];
  for (const [key, value] of inputs)
    dispatchArguments.push("-f", `${key}=${value}`);
  run("gh", dispatchArguments);

  const deadline = Date.now() + POLL_TIMEOUT_MS;
  while (Date.now() < deadline) {
    const matches = matchingRuns(
      listRuns(workflow),
      evidenceId,
      localSha,
      dispatchedAfter,
    );
    if (matches.length > 1)
      fail(`more than one workflow run matched evidence_id ${evidenceId}`);
    if (matches.length === 1) {
      const runId = matches[0].databaseId;
      if (!Number.isSafeInteger(runId) || runId <= 0)
        fail("matched run has an invalid numeric id");
      process.stdout.write(`${runId}\n`);
      return;
    }
    await sleep(POLL_INTERVAL_MS);
  }
  fail(`timed out waiting for workflow run with evidence_id ${evidenceId}`);
}

main().catch((error) => {
  process.stderr.write(`${error instanceof Error ? error.message : error}\n`);
  process.exitCode = 1;
});
