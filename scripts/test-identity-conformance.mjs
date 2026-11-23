#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";

const testName = "identity_conformance::identity_conformance_fastly";

export function runSuite(adapter, run = runCargo) {
  if (adapter === "axum")
    throw new Error("Axum identity runner awaits #1574 and #1600");
  if (adapter === "cloudflare" || adapter === "spin") {
    throw new Error(`${adapter} identity runner is not available yet`);
  }
  if (adapter !== "fastly") throw new Error(`Unknown adapter: ${adapter}`);
  const args = ["test-fastly", testName, "--locked"];
  const listed = run([...args, "--", "--list"]);
  if (!listed.stdout.split(/\r?\n/).includes(`${testName}: test`)) {
    throw new Error(`Missing required conformance test: ${testName}`);
  }
  const result = run([...args, "--", "--exact", "--nocapture"]);
  if (!/test result: ok\. 1 passed; 0 failed;/.test(result.stdout)) {
    throw new Error("Required conformance test did not execute successfully");
  }
  return result.stdout;
}

function runCargo(args) {
  const result = spawnSync("cargo", args, {
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(result.stderr || result.stdout || "Identity suite failed");
  }
  process.stderr.write(result.stderr);
  return result;
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  try {
    process.stdout.write(runSuite(process.argv[2] ?? "fastly"));
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
