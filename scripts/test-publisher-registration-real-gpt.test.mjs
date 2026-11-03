import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import {
  mkdtempSync,
  readFileSync,
  writeFileSync,
  mkdirSync,
  rmSync,
  existsSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createRequire } from "node:module";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";

const require = createRequire(import.meta.url);
const h = require("../crates/trusted-server-integration-tests/browser/publisher-registration/real-gpt.cjs");
const launcher = resolve("scripts/test-publisher-registration-real-gpt.mjs");
const inventory = {
  schemaVersion: 1,
  dedicatedTestInventory: true,
  authorizeGoogleTraffic: true,
  registered: {
    path: "/123456789/registered",
    tsCreativeId: 101,
    ordinaryCreativeId: 102,
    tsMarker: "Fixture publisher registration winner",
    ordinaryMarker: "Ordinary test creative",
  },
  unrelated: { path: "/123456789/unrelated" },
};
function isolated(callback) {
  const dir = mkdtempSync(join(tmpdir(), "real-gpt-launcher-"));
  try {
    return callback(dir);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}
function launch(dir, args) {
  const out = join(dir, "out");
  const result = spawnSync(
    process.execPath,
    [launcher, ...args, "--out", out],
    {
      encoding: "utf8",
      env: { ...process.env, PATH: "/no-runtime-tools-in-this-unit-test" },
    },
  );
  const outcome = JSON.parse(readFileSync(join(out, "outcome.json"), "utf8"));
  assert.equal(outcome.liveAcceptancePassed, false);
  assert.equal(outcome.originalFailureObserved, false);
  assert.deepEqual(outcome.counts, {
    total: 0,
    passed: 0,
    failed: 0,
    skipped: 0,
  });
  assert.equal(
    existsSync(join(out, "commands.json")),
    false,
    "preflight must not launch runtime, Docker, Cargo or Google",
  );
  return { result, outcome };
}

test("missing inventory is machine-readable blocked, nonzero, and starts no commands", () =>
  isolated((dir) => {
    const { result, outcome } = launch(dir, ["--preflight"]);
    assert.equal(result.status, 1);
    assert.equal(outcome.status, "blocked");
    assert.equal(outcome.mode, "preflight");
  }));

test("original-main artifacts missing is blocked, not a skip or current-mode surrogate", () =>
  isolated((dir) => {
    const file = join(dir, "inventory.json");
    writeFileSync(file, JSON.stringify(inventory));
    const { result, outcome } = launch(dir, [
      "--inventory",
      file,
      "--preflight",
    ]);
    assert.equal(result.status, 1);
    assert.equal(outcome.status, "blocked");
    assert.equal(outcome.baseline.originalFailureObserved, false);
  }));

test("structural preflight verifies original-main stamp but never claims live acceptance", () =>
  isolated((dir) => {
    const file = join(dir, "inventory.json");
    writeFileSync(file, JSON.stringify(inventory));
    const dist = join(dir, "dist");
    mkdirSync(dist);
    writeFileSync(
      join(dist, "tsjs-unit.js"),
      "unit-test-only artifact, never executable browser evidence",
    );
    const wasm = join(dir, "unit-test-only.wasm");
    writeFileSync(wasm, "not a runtime binary, preflight must not execute it");
    writeFileSync(
      `${wasm}.publisher-registration-baseline.json`,
      JSON.stringify({
        schemaVersion: 1,
        gitRevision: "51730bdaf" + "0".repeat(31),
        wasmSha256: h.sha256(readFileSync(wasm)),
        bundleSha256: h.distFingerprint(dist),
      }),
    );
    const args = [
      "--inventory",
      file,
      "--baseline-wasm",
      wasm,
      "--baseline-dist",
      dist,
      "--preflight",
    ];
    const valid = launch(dir, args);
    assert.equal(valid.result.status, 0);
    assert.equal(valid.outcome.status, "passed");
    writeFileSync(wasm, "changed unit-test bytes");
    const changed = launch(dir, args);
    assert.equal(changed.result.status, 1);
    assert.equal(changed.outcome.status, "failed");
  }));

test("ordinary Playwright discovery needs no real GPT inventory and excludes the dedicated gate", () => {
  const browser = resolve("crates/trusted-server-integration-tests/browser");
  const env = { ...process.env, PATH: "/no-runtime-tools-in-this-unit-test" };
  for (const key of Object.keys(env)) {
    if (key.startsWith("PUBLISHER_REGISTRATION_REAL_GPT_")) delete env[key];
  }
  const result = spawnSync(
    process.execPath,
    [
      join(browser, "node_modules/@playwright/test/cli.js"),
      "test",
      "--config",
      join(browser, "playwright.config.ts"),
      "--list",
      "--reporter=list",
    ],
    { cwd: browser, encoding: "utf8", env, timeout: 30_000 },
  );
  assert.equal(result.status, 0, result.stderr || result.stdout);
  assert.match(result.stdout, /Total: [1-9][0-9]* tests? in/);
  assert.doesNotMatch(
    result.stdout,
    /publisher-registration\.real-gpt\.spec\.ts/,
  );
});

test("post-spawn journal failure reaps its actual child before publishing failure", () =>
  isolated((dir) => {
    const out = join(dir, "out");
    mkdirSync(out);
    mkdirSync(join(out, "commands.json"));
    const file = join(dir, "inventory.json");
    writeFileSync(file, JSON.stringify(inventory));
    const observation = join(dir, "child-at-final-outcome.json");
    const observer = join(dir, "observe-owned-child.mjs");
    // Observe the actual spawn and final atomic outcome commit. No GPT, Docker,
    // provider or process response is substituted by this fault-injection test.
    writeFileSync(
      observer,
      `
import { createRequire, syncBuiltinESMExports } from "node:module";
const require = createRequire(import.meta.url);
const fs = require("node:fs");
const processes = require("node:child_process");
const spawn = processes.spawn;
const rename = fs.renameSync;
let pid;
processes.spawn = function (...args) {
  const child = spawn(...args);
  pid = child.pid;
  return child;
};
fs.renameSync = function (source, target) {
  if (target === ${JSON.stringify(join(out, "outcome.json"))} && JSON.parse(fs.readFileSync(source, "utf8")).status === "failed") {
    let alive = false;
    if (pid) {
      try { process.kill(pid, 0); alive = true; }
      catch (error) { if (error.code !== "ESRCH") throw error; }
    }
    fs.writeFileSync(${JSON.stringify(observation)}, JSON.stringify({ pid, alive }));
  }
  return rename(source, target);
};
syncBuiltinESMExports();
`,
    );
    const result = spawnSync(
      process.execPath,
      [
        "--import",
        observer,
        launcher,
        "--inventory",
        file,
        "--list",
        "--out",
        out,
      ],
      {
        encoding: "utf8",
        env: { ...process.env, PATH: "/no-runtime-tools-in-this-unit-test" },
        timeout: 15_000,
      },
    );
    assert.equal(result.status, 1, result.stderr || result.stdout);
    const final = JSON.parse(readFileSync(join(out, "outcome.json"), "utf8"));
    assert.equal(final.status, "failed");
    assert.equal(final.liveAcceptancePassed, false);
    assert.match(
      readFileSync(join(out, "failure.txt"), "utf8"),
      /EISDIR|ENOTDIR/,
    );
    const child = JSON.parse(readFileSync(observation, "utf8"));
    assert.ok(child.pid > 0, "fault must occur after actual child creation");
    assert.equal(
      child.alive,
      false,
      "the child must be reaped before final failure is published, not merely before launcher exit",
    );
    assert.throws(() => process.kill(child.pid, 0), { code: "ESRCH" });
  }));

for (const signal of ["SIGINT", "SIGTERM", "SIGKILL"]) {
  test(
    `${signal} during actual CLI discovery cannot retain a prior live success`,
    { timeout: 15_000 },
    async () => {
      const dir = mkdtempSync(join(tmpdir(), "real-gpt-interrupt-"));
      const out = join(dir, "out");
      mkdirSync(out);
      writeFileSync(
        join(out, "outcome.json"),
        JSON.stringify({
          status: "passed",
          started: "stale-prior-run",
          liveAcceptancePassed: true,
          originalFailureObserved: true,
        }),
      );
      const file = join(dir, "inventory.json");
      writeFileSync(file, JSON.stringify(inventory));
      const child = spawn(
        process.execPath,
        [launcher, "--inventory", file, "--list", "--out", out],
        {
          env: { ...process.env, PATH: "/no-runtime-tools-in-this-unit-test" },
          stdio: ["ignore", "pipe", "pipe"],
        },
      );
      let output = "";
      child.stdout.on("data", (bytes) => {
        output += bytes;
      });
      child.stderr.on("data", (bytes) => {
        output += bytes;
      });
      const closed = new Promise((resolve) =>
        child.once("close", (code, exitSignal) =>
          resolve({ code, signal: exitSignal }),
        ),
      );
      let commandPid;
      try {
        const deadline = AbortSignal.timeout(10_000);
        while (!commandPid) {
          assert.equal(child.exitCode, null, output);
          try {
            const running = JSON.parse(
              readFileSync(join(out, "outcome.json"), "utf8"),
            );
            const commands = JSON.parse(
              readFileSync(join(out, "commands.json"), "utf8"),
            );
            if (running.status === "running" && commands[0]?.pid) {
              assert.equal(running.liveAcceptancePassed, false);
              assert.equal(running.originalFailureObserved, false);
              assert.notEqual(running.started, "stale-prior-run");
              commandPid = commands[0].pid;
            }
          } catch (error) {
            if (error.code !== "ENOENT") throw error;
          }
          if (!commandPid) await delay(10, undefined, { signal: deadline });
        }
        child.kill(signal);
        const exit = await closed;
        const outcome = JSON.parse(
          readFileSync(join(out, "outcome.json"), "utf8"),
        );
        assert.equal(outcome.liveAcceptancePassed, false);
        assert.equal(outcome.originalFailureObserved, false);
        if (signal === "SIGKILL") {
          assert.equal(exit.signal, "SIGKILL");
          assert.equal(
            outcome.status,
            "running",
            "uncatchable termination must leave a non-passing incomplete invocation",
          );
        } else {
          assert.equal(exit.code, signal === "SIGINT" ? 130 : 143, output);
          assert.equal(outcome.status, "failed");
          assert.equal(outcome.signal, signal);
          assert.equal(outcome.exit, exit.code);
          assert.throws(
            () => process.kill(commandPid, 0),
            { code: "ESRCH" },
            "the actual owned discovery subprocess must be gone before the launcher exits",
          );
          const commands = JSON.parse(
            readFileSync(join(out, "commands.json"), "utf8"),
          );
          assert.equal(
            commands.length,
            1,
            "interruption cannot start another command",
          );
          assert.ok(
            commands[0].signal || commands[0].status !== null,
            "owned command must have a terminal record",
          );
        }
      } finally {
        // SIGKILL cannot unwind the launcher. The test owns this descendant and
        // cleans only the process group that the launcher recorded creating.
        if (commandPid) {
          try {
            process.kill(-commandPid, "SIGKILL");
          } catch (error) {
            if (error.code !== "ESRCH") throw error;
          }
        }
        if (child.exitCode === null && child.signalCode === null)
          child.kill("SIGKILL");
        await closed;
        rmSync(dir, { recursive: true, force: true });
      }
    },
  );
}
