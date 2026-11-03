#!/usr/bin/env node
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import {
  openSync,
  closeSync,
  existsSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
  appendFileSync,
  rmSync,
  renameSync,
} from "node:fs";
import { createServer } from "node:net";
import { dirname, join, resolve, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import { createRequire } from "node:module";
import { createHash, randomUUID } from "node:crypto";

const require = createRequire(import.meta.url);
const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const fixtures = join(root, "crates/trusted-server-integration-tests/fixtures");
const browser = join(root, "crates/trusted-server-integration-tests/browser");
const helpers = require(join(browser, "publisher-registration/real-gpt.cjs"));
let out;
let container;
let viceroy;
let started;
let mode = "run";
let blocked = false;
let liveAcceptancePassed = false;
let activeRunDir;
const runOutcomes = {
  candidate: {
    status: "blocked",
    counts: { total: 0, passed: 0, failed: 0, skipped: 0 },
  },
  baseline: {
    status: "blocked",
    counts: { total: 0, passed: 0, failed: 0, skipped: 0 },
    originalFailureObserved: false,
  },
};
const listedCounts = { candidate: 0, baseline: 0 };
const env = { ...process.env };
const commands = [];
const interrupted = new AbortController();
const ownedChildren = new Map();
const createdChildren = new WeakSet();
const childCleanup = new WeakMap();
let receivedSignal;
let signalCleanup = Promise.resolve();
let runtimeCleanup;

function atomicJson(path, value) {
  const temporary = `${path}.${process.pid}.tmp`;
  writeFileSync(temporary, JSON.stringify(value, null, 2));
  renameSync(temporary, path);
}

function rootOutcome(status, exit) {
  const counts = Object.values(runOutcomes).reduce(
    (sum, result) =>
      Object.fromEntries(
        Object.keys(sum).map((key) => [key, sum[key] + result.counts[key]]),
      ),
    { total: 0, passed: 0, failed: 0, skipped: 0 },
  );
  return {
    schemaVersion: 1,
    status,
    mode,
    started,
    finished: status === "running" ? undefined : new Date().toISOString(),
    exit,
    signal: receivedSignal,
    liveAcceptancePassed: status === "passed" && liveAcceptancePassed,
    originalFailureObserved:
      status === "running"
        ? false
        : runOutcomes.baseline.originalFailureObserved,
    counts,
    listedCounts,
    ...runOutcomes,
  };
}

function spawnOwned(command, args, options, settleOnInterrupt = false) {
  const grouped = process.platform !== "win32";
  const child = spawn(command, args, { ...options, detached: grouped });
  createdChildren.add(child);
  ownedChildren.set(child, { grouped, settleOnInterrupt });
  child.once("error", (error) => {
    child.launchError = error;
  });
  child.once("close", () => ownedChildren.delete(child));
  return child;
}

function stopOwned(child) {
  if (childCleanup.has(child)) return childCleanup.get(child);
  if (!ownedChildren.has(child)) return Promise.resolve();
  const { grouped } = ownedChildren.get(child);
  const stopped = (async () => {
    const closed = new Promise((resolve) => child.once("close", resolve));
    const kill = (signal) => {
      assert.ok(
        createdChildren.has(child),
        "only launcher-owned children may be signalled",
      );
      if (!child.pid) return;
      try {
        if (grouped) process.kill(-child.pid, signal);
        else child.kill(signal);
      } catch (error) {
        if (error.code !== "ESRCH") throw error;
      }
    };
    kill("SIGTERM");
    let timer;
    try {
      // Preserve the existing five-second cleanup grace before forced termination.
      await Promise.race([
        closed,
        new Promise((resolve) => {
          timer = setTimeout(resolve, 5000);
        }),
      ]);
      // The leader can exit before another member of its owned process group.
      kill("SIGKILL");
      await closed;
    } finally {
      clearTimeout(timer);
    }
  })();
  childCleanup.set(child, stopped);
  return stopped;
}

function handleSignal(signal) {
  if (receivedSignal) return;
  receivedSignal = signal;
  liveAcceptancePassed = false;
  interrupted.abort(new Error(`Interrupted by ${signal}`));
  signalCleanup = Promise.all(
    [...ownedChildren]
      .filter(([, ownership]) => !ownership.settleOnInterrupt)
      .map(([child]) => stopOwned(child)),
  );
  // The outer finally awaits this promise and records any cleanup failure.
  signalCleanup.catch(() => {});
}

async function run(
  command,
  args,
  {
    settleOnInterrupt = false,
    allowFailure = false,
    cleanup = false,
    returnResult = false,
    ...extra
  } = {},
) {
  if (!cleanup) interrupted.signal.throwIfAborted();
  const start = Date.now();
  const child = spawnOwned(
    command,
    args,
    { cwd: root, env, ...extra, stdio: ["ignore", "pipe", "pipe"] },
    settleOnInterrupt || cleanup,
  );
  let stdout = "";
  let stderr = "";
  child.stdout.on("data", (data) => {
    stdout += data;
  });
  child.stderr.on("data", (data) => {
    stderr += data;
  });
  const closed = new Promise((resolve) =>
    child.once("close", (code, signal) => resolve({ code, signal })),
  );
  try {
    const record = { command, args, pid: child.pid, status: null };
    commands.push(record);
    // Cleanup must still reach the daemon when the command journal is unwritable.
    // Its stop result is recorded separately in cleanup.json.
    if (out && !cleanup) atomicJson(join(out, "commands.json"), commands);
    const result = await closed;
    Object.assign(record, {
      status: result.code,
      signal: result.signal,
      elapsedMs: Date.now() - start,
      error: child.launchError?.message,
    });
    if (out && !cleanup) {
      appendFileSync(
        join(out, "commands.log"),
        `${JSON.stringify(record)}\n${stdout}${stderr}\n`,
      );
      atomicJson(join(out, "commands.json"), commands);
    }
    if (!cleanup) interrupted.signal.throwIfAborted();
    if (!allowFailure)
      assert.equal(
        result.code,
        0,
        `${command} failed: ${child.launchError?.message || stderr || stdout}`,
      );
    return returnResult
      ? { ...result, stdout, stderr, error: child.launchError }
      : stdout.trim();
  } catch (error) {
    // A failed journal write is still a failed command lifecycle. Keep resource
    // creation acknowledgements intact before the outer runtime cleanup runs.
    if (settleOnInterrupt || cleanup) await closed;
    else await stopOwned(child);
    throw error;
  }
}

function fingerprint(paths) {
  return run("bash", [
    "-c",
    'git ls-files --cached --others --exclude-standard -z -- "$@" ":!**/node_modules" ":!**/node_modules/**" | LC_ALL=C sort -z | xargs -0 sha256sum | sha256sum | awk \'{print $1}\'',
    "fingerprint",
    ...paths,
  ]);
}
const runtimePaths = [
  "Cargo.toml",
  "Cargo.lock",
  "rust-toolchain.toml",
  ".tool-versions",
  ".cargo",
  "crates/trusted-server-core",
  "crates/trusted-server-adapter-fastly",
  "crates/trusted-server-js",
  "crates/trusted-server-openrtb",
  ":!crates/trusted-server-js/lib/test/**",
];
function bundleFingerprint(dist) {
  return helpers.distFingerprint(dist);
}
function hashedFile(file) {
  return { path: resolve(file), sha256: helpers.sha256(readFileSync(file)) };
}
function consent() {
  const bytes = Buffer.alloc(32);
  const bits = (offset, width, value) => {
    const binary = value.toString(2).padStart(width, "0");
    for (let index = 0; index < width; index++)
      if (binary[index] === "1")
        bytes[(offset + index) >> 3] |= 1 << (7 - ((offset + index) % 8));
  };
  const now = Math.floor(Date.now() / 100);
  bits(0, 6, 2);
  bits(6, 36, now);
  bits(42, 36, now);
  bits(152, 24, 1 << 23);
  return bytes.toString("base64url");
}
async function json(url, options = {}) {
  const response = await fetch(url, {
    ...options,
    signal: AbortSignal.any([interrupted.signal, AbortSignal.timeout(30_000)]),
  });
  assert.ok(response.ok, `${url}: HTTP ${response.status}`);
  return response.json();
}
async function ready(url, child) {
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    interrupted.signal.throwIfAborted();
    if (child?.launchError) throw child.launchError;
    if (child && child.exitCode !== null)
      throw new Error("Viceroy exited before readiness");
    try {
      const response = await fetch(url, {
        signal: AbortSignal.any([
          interrupted.signal,
          AbortSignal.timeout(2000),
        ]),
      });
      if (response.ok) return;
    } catch {
      /* Readiness retry is not acceptance evidence. */
    }
    await delay(200, undefined, { signal: interrupted.signal });
  }
  throw new Error(`Startup readiness failed: ${url}`);
}
async function availablePort() {
  const server = createServer();
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const port = server.address().port;
  await new Promise((resolve) => server.close(resolve));
  return port;
}
async function preflight(baseUrl, origin, legacy, directory) {
  await json(`${origin}/api/fixture-auction`, { method: "DELETE" });
  const payloads = {};
  for (const variant of ["", "/no-bid"]) {
    const payload = await json(
      `${baseUrl}/_ts/page-bids?path=${encodeURIComponent(`/publisher-registration-real-gpt${variant}`)}`,
      {
        headers: {
          "X-TSJS-Page-Bids": "1",
          "Sec-Fetch-Site": "same-origin",
          "User-Agent": "Mozilla/5.0 FixturePublisherRegistration",
          Cookie: `euconsent-v2=${consent()}`,
        },
      },
    );
    assert.ok(
      payload.slots.some(
        (slot) =>
          slot.id === "fixture-header" &&
          (legacy
            ? slot.initialization === undefined
            : slot.initialization === "publisher_registered"),
      ),
    );
    if (!variant) {
      assert.equal(payload.bids["fixture-header"]?.hb_bidder, "fixture-bidder");
      assert.equal(
        payload.bids["fixture-header"]?.hb_adid,
        "fixture-win-fixture-header",
      );
      assert.match(
        payload.bids["fixture-header"]?.adm ?? "",
        /Fixture publisher registration winner/,
      );
      assert.equal(payload.bids["fixture-header"]?.debug_bid, undefined);
    } else assert.equal(Object.keys(payload.bids).length, 0);
    payloads[variant || "win"] = payload;
  }
  const provider = await json(`${origin}/api/fixture-auction`);
  assert.equal(
    provider.exchanges.length,
    2,
    "both actual Rust auctions must reach local fixture provider",
  );
  for (const exchange of provider.exchanges) {
    assert.equal(exchange.path, "/api/fixture-auction");
    assert.equal(exchange.request.imp[0].id, "fixture-header");
    assert.ok(exchange.request.imp[0].ext.prebid.bidder["fixture-bidder"]);
  }
  writeFileSync(
    join(directory, "provider-preflight.json"),
    JSON.stringify({ payloads, provider }, null, 2),
  );
}

function baselineArtifacts(opts) {
  assert.ok(
    opts["baseline-wasm"] && opts["baseline-dist"],
    "--baseline-wasm and --baseline-dist are required for original-main live closure",
  );
  const wasm = resolve(opts["baseline-wasm"]);
  const dist = resolve(opts["baseline-dist"]);
  const stamp = JSON.parse(
    readFileSync(`${wasm}.publisher-registration-baseline.json`, "utf8"),
  );
  assert.equal(stamp.schemaVersion, 1);
  assert.match(
    stamp.gitRevision,
    /^51730bdaf[0-9a-f]{31}$/,
    "baseline must be the original main revision, never current-mode simulation",
  );
  assert.equal(stamp.wasmSha256, helpers.sha256(readFileSync(wasm)));
  assert.equal(stamp.bundleSha256, helpers.distFingerprint(dist));
  if (stamp.sourceManifestSha256)
    assert.match(stamp.sourceManifestSha256, /^[a-f0-9]{64}$/);
  return { wasm, dist, stamp };
}

async function generateConfig({
  directory,
  inventory,
  prebidManifest,
  originPort,
  host,
  legacy,
}) {
  mkdirSync(directory, { recursive: true });
  const app = join(directory, "app.toml");
  const config = join(directory, "viceroy.toml");
  const mappings = join(directory, "provider-backends.toml");
  const predicted = join(directory, "provider-predicted.json");
  const origin = `http://127.0.0.1:${originPort}`;
  writeFileSync(
    app,
    helpers.appConfig(
      readFileSync(
        join(fixtures, "configs/trusted-server.publisher-registration.toml"),
        "utf8",
      ),
      inventory,
      prebidManifest,
      originPort,
      legacy,
    ),
  );
  const cargo = [
    "run",
    "-p",
    "trusted-server-integration-tests",
    "--target",
    host,
  ];
  await run("cargo", [
    ...cargo,
    "--bin",
    "generate-viceroy-config",
    "--",
    "--app-config",
    app,
    "--template",
    join(fixtures, "configs/viceroy-template.toml"),
    "--origin-url",
    origin,
    "--output",
    config,
  ]);
  await run("cargo", [
    ...cargo,
    "--example",
    "publisher_registration_backends",
    "--",
    app,
    origin,
    mappings,
    predicted,
  ]);
  const providerMappings = readFileSync(mappings, "utf8");
  assert.ok(
    !providerMappings.includes(new URL(inventory.gptScriptUrl).hostname),
    "Google must never map to local fixture provider",
  );
  appendFileSync(config, providerMappings);
  return {
    app: hashedFile(app),
    viceroy: hashedFile(config),
    mappings: hashedFile(mappings),
    predicted: hashedFile(predicted),
  };
}

function cleanupRuntime() {
  if (runtimeCleanup) return runtimeCleanup;
  const child = viceroy;
  const name = container;
  viceroy = undefined;
  container = undefined;
  runtimeCleanup = (async () => {
    if (child) await stopOwned(child);
    if (name) {
      // The unique name is reserved before docker run, including an interrupted launch.
      const inspected = await run("docker", ["container", "inspect", name], {
        cleanup: true,
        allowFailure: true,
        returnResult: true,
      });
      if (inspected.code !== 0) {
        assert.match(
          inspected.stderr || "",
          /No such (?:object|container)/i,
          `Docker ownership inspection failed: ${inspected.stderr}`,
        );
        return;
      }
      try {
        const logs = await run("docker", ["logs", name], {
          cleanup: true,
          allowFailure: true,
          returnResult: true,
        });
        writeFileSync(
          join(activeRunDir, "nextjs.log"),
          `${logs.stdout}${logs.stderr}`,
        );
      } finally {
        // Evidence-write failure must not leave the owned container running.
        const stopped = await run("docker", ["stop", name], {
          cleanup: true,
          allowFailure: true,
          returnResult: true,
          timeout: 15_000,
        });
        atomicJson(join(activeRunDir, "cleanup.json"), {
          container: name,
          exit: stopped.code,
          error: stopped.stderr || undefined,
        });
        assert.equal(
          stopped.code,
          0,
          `Docker cleanup failed: ${stopped.stderr}`,
        );
      }
    }
  })();
  return runtimeCleanup;
}

async function acceptanceRuntime({
  wasm,
  config,
  image,
  inventory,
  originPort,
  prebid,
  legacy,
}) {
  interrupted.signal.throwIfAborted();
  const name = legacy ? "baseline" : "candidate";
  activeRunDir = join(out, name);
  runtimeCleanup = undefined;
  mkdirSync(activeRunDir, { recursive: true });
  runOutcomes[name].status = "running";
  atomicJson(join(activeRunDir, "outcome.json"), runOutcomes[name]);
  const reportFile = join(activeRunDir, "browser/results.json");
  rmSync(reportFile, { force: true });
  const origin = `http://127.0.0.1:${originPort}`;
  let passed = false;
  try {
    container = `ts-registration-${process.pid}-${randomUUID()}`;
    await run(
      "docker",
      [
        "run",
        "--name",
        container,
        "-d",
        "--rm",
        "-p",
        `127.0.0.1:${originPort}:3000`,
        "-e",
        `ORIGIN_HOST=127.0.0.1:${originPort}`,
        "-e",
        `PUBLISHER_REGISTRATION_REAL_GPT_INVENTORY=${JSON.stringify(inventory)}`,
        "-e",
        `PUBLISHER_REGISTRATION_REAL_GPT_MODE=${legacy ? "legacy" : "registered"}`,
        image,
      ],
      {
        // Finish the daemon create/start acknowledgement before cleaning up its
        // reserved name. Killing this client could race a still-pending creation.
        settleOnInterrupt: true,
      },
    );
    await ready(`${origin}/api/fixture-auction`);
    const port = await availablePort();
    const stdout = openSync(join(activeRunDir, "viceroy.stdout.log"), "w");
    const stderr = openSync(join(activeRunDir, "viceroy.stderr.log"), "w");
    try {
      // OS-owned logs retain runtime evidence independently of the CLI event loop.
      interrupted.signal.throwIfAborted();
      viceroy = spawnOwned(
        "viceroy",
        [
          wasm,
          "-C",
          config,
          "--addr",
          `127.0.0.1:${port}`,
          "--log-stdout",
          "--log-stderr",
        ],
        {
          cwd: root,
          env: { ...env, RUST_LOG: "info" },
          stdio: ["ignore", stdout, stderr],
        },
      );
    } finally {
      closeSync(stdout);
      closeSync(stderr);
    }
    const baseUrl = `http://127.0.0.1:${port}`;
    atomicJson(join(activeRunDir, "runtime.json"), {
      container,
      viceroyPid: viceroy.pid,
      viceroyProcessGroup: process.platform !== "win32",
      baseUrl,
    });
    await ready(`${baseUrl}/health`, viceroy);
    await preflight(baseUrl, origin, legacy, activeRunDir);
    env.PUBLISHER_REGISTRATION_REAL_GPT_BASE_URL = baseUrl;
    env.PUBLISHER_REGISTRATION_REAL_GPT_ORIGIN = origin;
    env.PUBLISHER_REGISTRATION_REAL_GPT_RESULTS = join(activeRunDir, "browser");
    env.PUBLISHER_REGISTRATION_REAL_GPT_MODE = legacy ? "legacy" : "registered";
    env.PUBLISHER_REGISTRATION_PREBID_DIR = prebid;
    console.log(
      `${name}: actual-provider preflight passed. Starting explicitly authorized real GPT requests.`,
    );
    await run(
      process.execPath,
      [
        join(browser, "node_modules/@playwright/test/cli.js"),
        "test",
        "--config",
        join(browser, "playwright.publisher-registration.real-gpt.config.ts"),
      ],
      { cwd: browser },
    );
    passed = true;
  } finally {
    const summary = runOutcomes[name];
    summary.status = passed && !receivedSignal ? "passed" : "failed";
    try {
      if (existsSync(reportFile)) {
        const report = JSON.parse(readFileSync(reportFile, "utf8"));
        summary.counts = {
          total:
            report.stats.expected +
            report.stats.unexpected +
            report.stats.skipped +
            report.stats.flaky,
          passed: report.stats.expected,
          failed: report.stats.unexpected + report.stats.flaky,
          skipped: report.stats.skipped,
        };
      }
    } catch (error) {
      passed = false;
      summary.status = "failed";
      summary.reportError = String(error);
    }
    if (legacy)
      summary.originalFailureObserved =
        passed && summary.counts.passed === 1 && summary.counts.skipped === 0;
    atomicJson(join(activeRunDir, "outcome.json"), summary);
    await cleanupRuntime();
  }
  assert.ok(
    runOutcomes[name].counts.total > 0 &&
      runOutcomes[name].counts.passed === runOutcomes[name].counts.total &&
      runOutcomes[name].counts.skipped === 0,
    `${name} requires positive passing tests and zero skips`,
  );
}

async function main() {
  const argv = process.argv.slice(2);
  mode = argv.includes("--preflight")
    ? "preflight"
    : argv.includes("--list")
      ? "list"
      : argv.includes("--prepare")
        ? "prepare"
        : "run";
  started = new Date().toISOString();
  const outIndex = argv.indexOf("--out");
  if (
    outIndex >= 0 &&
    argv[outIndex + 1] &&
    !argv[outIndex + 1].startsWith("--")
  ) {
    out = resolve(argv[outIndex + 1]);
    mkdirSync(out, { recursive: true });
    // Invalidate any prior success before reading inputs, building or owning resources.
    atomicJson(join(out, "outcome.json"), rootOutcome("running", null));
  }
  const inventoryIndex = argv.indexOf("--inventory");
  blocked =
    inventoryIndex < 0 ||
    !argv[inventoryIndex + 1] ||
    !existsSync(resolve(argv[inventoryIndex + 1]));
  const opts = helpers.options(argv);
  const inventory = helpers.loadInventory(opts.inventory);
  env.PUBLISHER_REGISTRATION_REAL_GPT_INVENTORY_FILE = resolve(opts.inventory);
  if (opts.list) {
    for (const legacy of [false, true]) {
      env.PUBLISHER_REGISTRATION_REAL_GPT_MODE = legacy
        ? "legacy"
        : "registered";
      const listing = await run(
        process.execPath,
        [
          join(browser, "node_modules/@playwright/test/cli.js"),
          "test",
          "--config",
          join(browser, "playwright.publisher-registration.real-gpt.config.ts"),
          "--list",
        ],
        { cwd: browser },
      );
      console.log(listing);
      listedCounts[legacy ? "baseline" : "candidate"] = Number(
        listing.match(/Total: (\d+) tests?/)?.[1] || 0,
      );
    }
  }
  if (!opts["baseline-wasm"] && !opts.prepare) {
    blocked = true;
    throw new Error(
      "Original-main baseline artifacts absent: --baseline-wasm and --baseline-dist are required; no live closure can pass",
    );
  }
  if (
    opts["baseline-wasm"] &&
    (!existsSync(resolve(opts["baseline-wasm"])) ||
      !existsSync(
        `${resolve(opts["baseline-wasm"])}.publisher-registration-baseline.json`,
      ) ||
      !existsSync(resolve(opts["baseline-dist"])))
  )
    blocked = true;
  const baseline = opts["baseline-wasm"] ? baselineArtifacts(opts) : undefined;
  if (opts.preflight || opts.list) {
    console.log(
      "Dedicated inventory and original-main artifact provenance are structurally valid. No runtime started, no Google requests made, no live acceptance passed.",
    );
    return;
  }
  out = resolve(opts.out);
  mkdirSync(out, { recursive: true });
  writeFileSync(
    join(out, "inventory.json"),
    JSON.stringify(inventory, null, 2),
  );
  const dist = resolve(opts.dist);
  const prebid = resolve(opts.prebid);
  const prebidManifest = JSON.parse(
    readFileSync(join(prebid, "manifest.json"), "utf8"),
  );
  assert.equal(prebidManifest.schemaVersion, 1);
  assert.equal(basename(prebidManifest.filename), prebidManifest.filename);
  const prebidBytes = readFileSync(join(prebid, prebidManifest.filename));
  assert.equal(helpers.sha256(prebidBytes), prebidManifest.sha256);
  assert.equal(
    `sha384-${createHash("sha384").update(prebidBytes).digest("base64")}`,
    prebidManifest.sri,
  );
  writeFileSync(
    join(out, "prebid-manifest.json"),
    JSON.stringify(prebidManifest, null, 2),
  );
  for (const file of ["tsjs-core.js", "tsjs-gpt.js", "tsjs-prebid.js"])
    assert.ok(existsSync(join(dist, file)), `missing current bundle ${file}`);
  env.TSJS_PREBUILT_DIR = dist;
  const runtimeSource = await fingerprint(runtimePaths);
  const bundleSource = bundleFingerprint(dist);
  const rustc = await run("rustc", ["-Vv"]);
  const sourceSha256 = helpers.sha256(
    `${runtimeSource}\n${rustc}\n${bundleSource}\n`,
  );
  const wasm = resolve(
    opts.wasm ||
      join(
        process.env.CARGO_TARGET_DIR || join(root, "target"),
        "wasm32-wasip1/release/trusted-server-adapter-fastly.wasm",
      ),
  );
  const stampFile = `${wasm}.publisher-registration.json`;
  if (!opts.wasm) {
    await run("cargo", [
      "build",
      "--package",
      "trusted-server-adapter-fastly",
      "--release",
      "--target",
      "wasm32-wasip1",
    ]);
    assert.equal(
      await fingerprint(runtimePaths),
      runtimeSource,
      "runtime source changed during build",
    );
    writeFileSync(
      stampFile,
      JSON.stringify(
        {
          schemaVersion: 1,
          sourceSha256,
          bundleSha256: bundleSource,
          wasmSha256: helpers.sha256(readFileSync(wasm)),
          rustc,
          bundleMode: "prebuilt",
          target: "wasm32-wasip1",
        },
        null,
        2,
      ),
    );
  }
  const stamp = JSON.parse(readFileSync(stampFile, "utf8"));
  assert.equal(stamp.schemaVersion, 1);
  assert.equal(
    stamp.sourceSha256,
    sourceSha256,
    "provided WASM source stamp is stale; rebuild and stamp with the existing launcher or omit --wasm",
  );
  assert.equal(
    stamp.bundleSha256,
    bundleSource,
    "provided WASM has different JS bundles",
  );
  assert.equal(stamp.rustc, rustc);
  assert.equal(stamp.bundleMode, "prebuilt");
  assert.equal(stamp.wasmSha256, helpers.sha256(readFileSync(wasm)));
  writeFileSync(join(out, "wasm-build.json"), JSON.stringify(stamp, null, 2));
  const originPort = Number(opts["origin-port"] || "8888");
  const host = rustc.match(/^host: (.+)$/m)?.[1];
  assert.ok(host, "rustc host target is required");
  const candidateConfig = await generateConfig({
    directory: join(out, "candidate"),
    inventory,
    prebidManifest,
    originPort,
    host,
    legacy: false,
  });
  const baselineConfig = baseline
    ? await generateConfig({
        directory: join(out, "baseline"),
        inventory,
        prebidManifest,
        originPort,
        host,
        legacy: true,
      })
    : undefined;
  const imageSource = await fingerprint([
    ".tool-versions",
    "crates/trusted-server-integration-tests/fixtures/frameworks/nextjs",
  ]);
  const image =
    opts.image ||
    `test-nextjs:publisher-registration-real-gpt-${imageSource.slice(0, 16)}`;
  if (!opts.image) {
    const label = await run(
      "docker",
      [
        "image",
        "inspect",
        "--format",
        '{{index .Config.Labels "org.trusted-server.fixture-source"}}',
        image,
      ],
      { allowFailure: true },
    );
    if (label !== imageSource) {
      const node = readFileSync(join(root, ".tool-versions"), "utf8").match(
        /^nodejs\s+(\S+)/m,
      )?.[1];
      assert.ok(node);
      await run("docker", [
        "build",
        "--build-arg",
        `NODE_VERSION=${node}`,
        "--label",
        `org.trusted-server.fixture-source=${imageSource}`,
        "-t",
        image,
        join(fixtures, "frameworks/nextjs"),
      ]);
    }
  }
  const imageInfo = JSON.parse(
    await run("docker", ["image", "inspect", image]),
  )[0];
  assert.equal(
    imageInfo.Config.Labels["org.trusted-server.fixture-source"],
    imageSource,
    "provided Next.js image source label is stale",
  );
  writeFileSync(
    join(out, "artifacts.json"),
    JSON.stringify(
      {
        schemaVersion: 1,
        candidate: { wasm: hashedFile(wasm), ...candidateConfig },
        baseline: baseline
          ? {
              wasm: hashedFile(baseline.wasm),
              stamp: baseline.stamp,
              ...baselineConfig,
            }
          : null,
        prebid: hashedFile(join(prebid, prebidManifest.filename)),
        inventory: hashedFile(opts.inventory),
        image: {
          reference: image,
          id: imageInfo.Id,
          sourceSha256: imageSource,
        },
        runtimeSource,
        bundleSource,
        node: process.version,
        rustc,
        git: await run("git", ["rev-parse", "HEAD"]),
      },
      null,
      2,
    ),
  );
  if (opts.prepare) {
    console.log(
      `Prepared actual Rust/Next.js/provider artifacts in ${out}. No Google traffic or live acceptance executed.`,
    );
    return;
  }
  const expectedViceroy = readFileSync(
    join(root, ".tool-versions"),
    "utf8",
  ).match(/^viceroy\s+(\S+)/m)?.[1];
  assert.equal(
    await run("viceroy", ["--version"]),
    `viceroy ${expectedViceroy}`,
    "use repository-pinned Viceroy",
  );
  await acceptanceRuntime({
    wasm: baseline.wasm,
    config: baselineConfig.viceroy.path,
    image,
    inventory,
    originPort,
    prebid,
    legacy: true,
  });
  await acceptanceRuntime({
    wasm,
    config: candidateConfig.viceroy.path,
    image,
    inventory,
    originPort,
    prebid,
    legacy: false,
  });
  liveAcceptancePassed =
    runOutcomes.baseline.originalFailureObserved &&
    runOutcomes.candidate.status === "passed";
  console.log(
    `Real GPT local acceptance passed; evidence: ${out}. This is not deployed production validation.`,
  );
}

for (const signal of ["SIGINT", "SIGTERM"])
  process.on(signal, () => handleSignal(signal));

let status = 0;
try {
  await main();
} catch (error) {
  status = 1;
  console.error(error.stack || String(error));
  if (out)
    writeFileSync(join(out, "failure.txt"), error.stack || String(error));
} finally {
  try {
    await signalCleanup;
  } catch (error) {
    status = 1;
    console.error(String(error));
  }
  try {
    await cleanupRuntime();
  } catch (error) {
    status = 1;
    console.error(String(error));
  }
  if (receivedSignal) status = receivedSignal === "SIGINT" ? 130 : 143;
  const outcome = rootOutcome(
    status === 0
      ? "passed"
      : receivedSignal
        ? "failed"
        : blocked
          ? "blocked"
          : "failed",
    status,
  );
  if (out) atomicJson(join(out, "outcome.json"), outcome);
  console.log(JSON.stringify(outcome));
}
process.exitCode = status;
