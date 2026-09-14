import assert from "node:assert/strict";
import { execFile as execFileCallback } from "node:child_process";
import { existsSync } from "node:fs";
import {
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";

const scriptsDirectory = path.dirname(fileURLToPath(import.meta.url));
const harnessPath = path.join(scriptsDirectory, "template-cache-local-test.sh");
const execFile = promisify(execFileCallback);

function pathWithoutViceroy(firstDirectory) {
  const filtered = (process.env.PATH ?? "")
    .split(path.delimiter)
    .filter(
      (directory) => directory && !existsSync(path.join(directory, "viceroy")),
    )
    .join(path.delimiter);
  return `${firstDirectory}${path.delimiter}${filtered}`;
}

async function runHarnessWithToolVersions(toolVersions) {
  const repositoryRoot = await mkdtemp(
    path.join(os.tmpdir(), "trusted-server-viceroy-pin-"),
  );
  const copiedHarness = path.join(
    repositoryRoot,
    "scripts",
    "template-cache-local-test.sh",
  );
  const testBin = path.join(repositoryRoot, "test-bin");
  const { stdout: rustSysroot } = await execFile("rustc", [
    "--print",
    "sysroot",
  ]);
  await mkdir(path.dirname(copiedHarness), { recursive: true });
  await mkdir(testBin);
  await copyFile(harnessPath, copiedHarness);
  await symlink(
    path.join(rustSysroot.trim(), "bin", "rustc"),
    path.join(testBin, "rustc"),
  );
  if (toolVersions !== undefined) {
    await writeFile(
      path.join(repositoryRoot, ".tool-versions"),
      toolVersions,
      "utf8",
    );
  }

  try {
    const result = await execFile("/bin/bash", [copiedHarness], {
      env: { ...process.env, PATH: pathWithoutViceroy(testBin) },
    });
    return { ...result, code: 0 };
  } catch (error) {
    return error;
  } finally {
    await rm(repositoryRoot, { recursive: true, force: true });
  }
}

test("template-cache harness derives its Viceroy install hint from .tool-versions", async () => {
  const harness = await readFile(harnessPath, "utf8");

  assert.match(
    harness,
    /VICEROY_VERSION="\$\([\s\S]*?awk '[\s\S]*?\$1 == "viceroy"[\s\S]*?"\$REPO_ROOT\/\.tool-versions"[\s\S]*?\)"/u,
  );
  assert.match(
    harness,
    /if \[ -z "\$VICEROY_VERSION" \]; then[\s\S]*?Viceroy pin is missing or malformed/u,
  );
  assert.match(
    harness,
    /cargo install viceroy --version \$VICEROY_VERSION --locked/u,
  );
  assert.doesNotMatch(harness, /cargo install viceroy --version [0-9]/u);
});

test("template-cache harness reports the exact valid Viceroy pin", async () => {
  const result = await runHarnessWithToolVersions(
    "nodejs 24.12.0\nviceroy 9.8.7\n",
  );

  assert.equal(result.code, 1);
  assert.match(
    result.stderr,
    /cargo install viceroy --version 9\.8\.7 --locked/u,
  );
});

test("template-cache harness rejects a missing Viceroy pin", async () => {
  const result = await runHarnessWithToolVersions("nodejs 24.12.0\n");

  assert.equal(result.code, 1);
  assert.match(result.stderr, /Viceroy pin is missing or malformed/u);
});

test("template-cache harness rejects malformed and duplicate Viceroy pins", async () => {
  for (const toolVersions of [
    "viceroy latest\n",
    "viceroy 1.2.3 extra\n",
    "viceroy 1.2.3\nviceroy 4.5.6\n",
  ]) {
    const result = await runHarnessWithToolVersions(toolVersions);

    assert.equal(result.code, 1);
    assert.match(result.stderr, /Viceroy pin is missing or malformed/u);
  }
});

test("template-cache harness rejects an unreadable tool-version source", async () => {
  const result = await runHarnessWithToolVersions(undefined);

  assert.equal(result.code, 1);
  assert.match(result.stderr, /Unable to read Viceroy pin/u);
});
