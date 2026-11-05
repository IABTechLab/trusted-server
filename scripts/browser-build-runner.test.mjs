import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { copyFileSync, existsSync, mkdtempSync, readFileSync, writeFileSync, mkdirSync, rmSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { Worker } from "node:worker_threads";

const fixture = new URL("./browser-build-fixtures/process.mjs", import.meta.url).pathname;
const leaseName = ".browser-build-lease";
const nonceName = "TS_BROWSER_BUILD_LEASE";
const npm = path.resolve(path.dirname(process.execPath), "../lib/node_modules/npm/bin/npm-cli.js");

function launch(root, args, env = {}) {
  const child = spawn(process.execPath, [fixture, "run", root, ...args], {
    env: { ...process.env, [nonceName]: "", ...env },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stdout = "";
  let stderr = "";
  child.stdout.on("data", (data) => { stdout += data; });
  child.stderr.on("data", (data) => { stderr += data; });
  child.result = new Promise((resolve) => child.on("exit", (code, signal) => resolve({ code, signal, stdout, stderr })));
  return child;
}
function command(mode, root, ...args) {
  return [process.execPath, fixture, mode, root, ...args];
}
async function until(check) {
  // Test watchdog only. Lease acquisition and child draining have no deadline.
  for (let n = 0; n < 500; n++) {
    if (check()) return;
    await delay(20);
  }
  throw new Error("Fixture did not reach its synchronization point within 10s");
}
const originalOwners = new Map();
function owner(root) {
  const record = JSON.parse(readFileSync(path.join(root, leaseName, "owner.json"), "utf8"));
  if (!originalOwners.has(root)) originalOwners.set(root, structuredClone(record));
  return record;
}
function processStat(pid) {
  try {
    const text = readFileSync(`/proc/${pid}/stat`, "utf8");
    const fields = text.slice(text.lastIndexOf(")") + 2).trim().split(/\s+/);
    return { state: fields[0], start: fields[19], group: Number(fields[2]), session: Number(fields[3]) };
  } catch (error) { if (error.code === "ENOENT") return undefined; throw error; }
}
async function project(t) {
  const root = mkdtempSync(path.join(tmpdir(), "browser-build-lease-"));
  t.after(() => {
    // Never signal from metadata a test deliberately corrupted. Keep the
    // original observed identities, and target only this fixture's processes.
    const record = originalOwners.get(root) ?? (existsSync(path.join(root, leaseName, "owner.json")) ? owner(root) : undefined);
    if (record) {
      for (const [pid, start] of [[record.scope.leaderPid, record.scope.start], [record.holder.pid, record.holder.start]]) {
        if (pid > 1 && pid !== process.pid && processStat(pid)?.start === start) {
          try { process.kill(pid, "SIGKILL"); } catch (error) { if (error.code !== "ESRCH") throw error; }
        }
      }
      for (const name of ["ready", "leaf-ready", "parent-ready"]) {
        const file = path.join(root, name);
        if (!existsSync(file)) continue;
        const pid = Number(readFileSync(file, "utf8"));
        const info = processStat(pid);
        if (pid > 1 && info?.group === record.scope.group && info.session === record.scope.session) {
          try { process.kill(pid, "SIGKILL"); } catch (error) { if (error.code !== "ESRCH") throw error; }
        }
      }
    }
    originalOwners.delete(root);
    rmSync(root, { recursive: true, force: true });
  });
  return root;
}
async function held(t) {
  const root = await project(t);
  const ready = path.join(root, "ready");
  const release = path.join(root, "release");
  const child = launch(root, command("hold", root, ready, release));
  await until(() => existsSync(ready));
  return { root, child, release, record: owner(root) };
}

async function stale(t) {
  const root = await project(t);
  const preload = new URL("./browser-build-fixtures/pause-retirement.mjs", import.meta.url).href;
  const child = launch(root, command("exit", root, "0"), {
    LEASE_FIXTURE_RETIREMENT_ROOT: root,
    NODE_OPTIONS: `--import=${preload}`,
  });
  await until(() => existsSync(path.join(root, "retiring")));
  const record = owner(root);
  child.kill("SIGKILL");
  await child.result;
  assert.throws(() => process.kill(-record.scope.group, 0), { code: "ESRCH" }, "fixture group must really be absent");
  return { root, record };
}

// Each test owns isolated directories and subprocesses. Run serially to make
// signal and orphan failures attributable without touching the browser project.
test("public CLI uses the one project root relative to its checkout", { timeout: 15000 }, async (t) => {
  const checkout = await project(t);
  const root = path.join(checkout, "crates/trusted-server-js/lib");
  const scripts = path.join(checkout, "scripts");
  mkdirSync(root, { recursive: true });
  mkdirSync(scripts);
  for (const name of ["browser-build-runner.mjs", "browser-build-lease.mjs"]) {
    copyFileSync(new URL(`./${name}`, import.meta.url), path.join(scripts, name));
  }
  const child = spawn(process.execPath, [path.join(scripts, "browser-build-runner.mjs"), "--", ...command("guard", root)], {
    cwd: tmpdir(), stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, [nonceName]: "" },
  });
  let output = "";
  child.stdout.on("data", (data) => { output += data; });
  child.stderr.on("data", (data) => { output += data; });
  assert.equal(await new Promise((resolve) => child.on("exit", resolve)), 0, output);
  assert.match(output, /guard passed/);
  assert.equal(existsSync(path.join(root, leaseName)), false);
});

test("Cargo -> runner -> npm -> nested runner -> guarded Node", { timeout: 15000 }, async (t) => {
  const root = await project(t);
  writeFileSync(path.join(root, "package.json"), JSON.stringify({
    private: true,
    scripts: { nested: `"${process.execPath}" "${fixture}" nested "${root}" valid` },
  }));
  const child = spawn(process.execPath, [fixture, "cargo", root, npm], {
    stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, [nonceName]: "" },
  });
  let output = "";
  child.stdout.on("data", (data) => { output += data; });
  child.stderr.on("data", (data) => { output += data; });
  const code = await new Promise((resolve) => child.on("exit", resolve));
  assert.equal(code, 0, output);
  assert.match(output, /guard passed/);
  assert.equal(existsSync(path.join(root, leaseName)), false);
});

for (const mode of ["missing", "wrong"]) {
  test(`nested ${mode} nonce fails rather than acquiring`, { timeout: 15000 }, async (t) => {
    const root = await project(t);
    const result = await launch(root, command("nested", root, mode)).result;
    assert.equal(result.code, 1, JSON.stringify(result));
    assert.match(result.stderr, /nonce|token/i);
    assert.equal(existsSync(path.join(root, leaseName)), false);
  });
}

test("a copied live nonce outside the holder ancestry is rejected", { timeout: 15000 }, async (t) => {
  const { root, child, release, record } = await held(t);
  const result = await launch(root, command("exit", root, "0"), { [nonceName]: record.nonce }).result;
  assert.equal(result.code, 1);
  assert.match(result.stderr, /scope|ancestry/i);
  assert.equal(owner(root).nonce, record.nonce);
  writeFileSync(release, "");
  assert.equal((await child.result).code, 0);
});

test("guard refuses direct unleased invocation", { timeout: 15000 }, async (t) => {
  const root = await project(t);
  const child = spawn(process.execPath, [fixture, "guard", root], { stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, [nonceName]: "" } });
  let stderr = "";
  child.stderr.on("data", (data) => { stderr += data; });
  assert.equal(await new Promise((resolve) => child.on("exit", resolve)), 1);
  assert.match(stderr, /Browser build lease metadata missing; invoke browser-build-runner\.mjs/);
});

test("concurrent outer commands serialize their whole operation", { timeout: 15000 }, async (t) => {
  const root = await project(t);
  const log = path.join(root, "order");
  const children = Array.from({ length: 5 }, () => launch(root, command("log", root, log)));
  const results = await Promise.all(children.map((child) => child.result));
  for (const result of results) assert.equal(result.code, 0, JSON.stringify(result));
  const lines = readFileSync(log, "utf8").trim().split("\n");
  assert.equal(lines.length, 10);
  for (let n = 0; n < lines.length; n += 2) {
    assert.match(lines[n], /^start /);
    assert.equal(lines[n + 1], lines[n].replace("start", "end"));
  }
  assert.equal(existsSync(path.join(root, leaseName)), false);
});

test("command errors clean up and preserve exit status", { timeout: 15000 }, async (t) => {
  const root = await project(t);
  assert.equal((await launch(root, command("exit", root, "7")).result).code, 7);
  assert.equal(existsSync(path.join(root, leaseName)), false);
  assert.equal((await launch(root, [path.join(root, "missing-command")]).result).code, 1);
  assert.equal(existsSync(path.join(root, leaseName)), false);
});

for (const [signal, expectedCode] of [["SIGTERM", 143], ["SIGINT", 130], ["SIGHUP", 129]]) {
  test(`${signal} forwards to the scope and cleans up after children exit`, { timeout: 15000 }, async (t) => {
    const { root, child } = await held(t);
    child.kill(signal);
    const result = await child.result;
    assert.equal(result.code, expectedCode, JSON.stringify(result));
    assert.equal(existsSync(path.join(root, leaseName)), false);
  });
}

test("dead holder with a surviving orphan is never recovered", { timeout: 15000 }, async (t) => {
  const { root, child, release, record } = await held(t);
  process.kill(record.holder.pid, "SIGKILL");
  await child.result;
  const result = await launch(root, command("exit", root, "0")).result;
  assert.equal(result.code, 1);
  assert.match(result.stderr, /scope.*alive|orphan/i);
  assert.equal(owner(root).nonce, record.nonce);
  writeFileSync(release, "");
});

test("outer holder retains ownership after its command leaves an orphan", { timeout: 15000 }, async (t) => {
  const root = await project(t);
  const ready = path.join(root, "ready");
  const release = path.join(root, "release");
  const child = launch(root, command("orphan", root, ready, release));
  await until(() => existsSync(ready));
  await delay(100);
  assert.equal(child.exitCode, null);
  assert.ok(owner(root).scope.session);
  writeFileSync(release, "");
  assert.equal((await child.result).code, 0);
});

test("cancellation after the scope leader exits returns promptly with the orphan lease retained", { timeout: 15000 }, async (t) => {
  const root = await project(t);
  const ready = path.join(root, "ready");
  const release = path.join(root, "release");
  const child = launch(root, command("orphan", root, ready, release));
  await until(() => existsSync(ready));
  const record = owner(root);
  await until(() => processStat(record.scope.leaderPid) === undefined);
  assert.equal(process.kill(-record.scope.group, 0), true);
  child.kill("SIGTERM");
  const result = await child.result;
  assert.equal(result.code, 143, JSON.stringify(result));
  assert.match(result.stderr, /interrupted.*retaining lease/);
  assert.equal(owner(root).nonce, record.nonce);
  assert.equal(process.kill(-record.scope.group, 0), true, "interruption must not masquerade as an empty group");
  const contender = await launch(root, command("exit", root, "0")).result;
  assert.equal(contender.code, 1);
  assert.match(contender.stderr, /orphan/);
  writeFileSync(release, "");
  await until(() => {
    try { process.kill(-record.scope.group, 0); return false; }
    catch (error) { if (error.code === "ESRCH") return true; throw error; }
  });
});

test("a fork after enumeration cannot retire the lease while its same-group leaf lives", { timeout: 15000 }, async (t) => {
  const root = await project(t);
  const preload = new URL("./browser-build-fixtures/pause-scope-proof.mjs", import.meta.url).href;
  const child = launch(root, command("fork-race-command", root), {
    LEASE_FIXTURE_FORK_RACE_ROOT: root,
    NODE_OPTIONS: `--import=${preload}`,
  });
  let leafIdentity;
  t.after(() => {
    if (leafIdentity) {
      try {
        const fields = readFileSync(`/proc/${leafIdentity.pid}/stat`, "utf8").split(") ")[1].split(" ");
        if (fields[19] === leafIdentity.start) process.kill(leafIdentity.pid, "SIGKILL");
      } catch (error) { if (error.code !== "ENOENT" && error.code !== "ESRCH") throw error; }
    }
    if (child.exitCode === null) child.kill("SIGKILL");
  });
  await until(() => existsSync(path.join(root, "proof-ready")));
  const record = owner(root);
  writeFileSync(path.join(root, "fork"), "");
  await until(() => existsSync(path.join(root, "leaf-ready")));
  const parent = readFileSync(path.join(root, "parent-ready"), "utf8");
  await until(() => {
    try { return readFileSync(`/proc/${parent}/stat`, "utf8").split(") ")[1].startsWith("Z"); }
    catch (error) { if (error.code === "ENOENT") return true; throw error; }
  });
  writeFileSync(path.join(root, "proof-continue"), "");
  await until(() => child.exitCode !== null || (
    existsSync(path.join(root, "group-live-probes")) &&
    Number(readFileSync(path.join(root, "group-live-probes"), "utf8")) >= 2
  ));
  const leaf = readFileSync(path.join(root, "leaf-ready"), "utf8");
  const fields = readFileSync(`/proc/${leaf}/stat`, "utf8").split(") ")[1].split(" ");
  leafIdentity = { pid: Number(leaf), start: fields[19] };
  assert.equal(Number(fields[2]), record.scope.group, "ordinary grandchild must remain in the recorded process group");
  assert.notEqual(fields[0], "Z", "the leaf must still be executing at the proof boundary");
  assert.equal(existsSync(path.join(root, leaseName)), true, "lease retired while the same-group leaf was alive");
  assert.equal(child.exitCode, null, "outer runner must wait for kernel group absence");
  writeFileSync(path.join(root, "leaf-stop"), "");
  assert.equal((await child.result).code, 0);
});

test("safe stale recovery waits for absence over the grace interval", { timeout: 15000 }, async (t) => {
  const { root } = await stale(t);
  const started = performance.now();
  const result = await launch(root, command("exit", root, "0")).result;
  assert.equal(result.code, 0, JSON.stringify(result));
  assert.ok(performance.now() - started >= 1000, "stale recovery must observe the full 1s grace");
  assert.equal(existsSync(path.join(root, leaseName)), false);
});

test("a reused holder PID fails closed even when its old scope is absent", { timeout: 15000 }, async (t) => {
  const { root, record } = await stale(t);
  record.holder = { pid: process.pid, start: "0" };
  writeFileSync(path.join(root, leaseName, "owner.json"), JSON.stringify(record));
  const result = await launch(root, command("exit", root, "0")).result;
  assert.equal(result.code, 1);
  assert.match(result.stderr, /PID.*reus|start.*identity/i);
  assert.equal(owner(root).nonce, record.nonce);
});

test("publication precedes every tool and a racing contender cannot recover the gap", { timeout: 15000 }, async (t) => {
  const root = await project(t);
  const ready = path.join(root, "ready");
  const release = path.join(root, "release");
  const preload = new URL("./browser-build-fixtures/pause-publication.mjs", import.meta.url).href;
  const holder = launch(root, command("hold", root, ready, release), {
    LEASE_FIXTURE_PUBLICATION_ROOT: root,
    NODE_OPTIONS: `--import=${preload}`,
  });
  await until(() => existsSync(path.join(root, "publishing")));
  assert.equal(existsSync(ready), false, "no tool can start before complete metadata publication");
  assert.equal(existsSync(path.join(root, leaseName, "owner.json")), false);
  const pending = JSON.parse(readFileSync(path.join(root, leaseName, "owner.pending"), "utf8"));
  assert.equal(processStat(pending.scope.leaderPid).start, pending.scope.start, "dedicated scope must already exist behind the gate");
  const contender = launch(root, command("log", root, path.join(root, "order")));
  await delay(100);
  assert.equal(contender.exitCode, null);
  assert.equal(existsSync(ready), false);
  writeFileSync(path.join(root, "publish"), "");
  await until(() => existsSync(ready));
  const record = owner(root);
  assert.equal(record.version, 2);
  assert.notEqual(record.scope.leaderPid, record.holder.pid);
  assert.equal(record.scope.session, record.scope.leaderPid);
  assert.equal(record.scope.group, record.scope.leaderPid);
  assert.equal(record.scope.start, processStat(record.scope.leaderPid).start);
  assert.notEqual(processStat(record.holder.pid).group, record.scope.group);
  writeFileSync(release, "");
  assert.equal((await holder.result).code, 0);
  const result = await contender.result;
  assert.equal(result.code, 0, JSON.stringify(result));
});

test("concurrent stale reapers cannot remove a successor's lease", { timeout: 15000 }, async (t) => {
  const { root } = await stale(t);
  const log = path.join(root, "order");
  const children = Array.from({ length: 4 }, () => launch(root, command("log", root, log)));
  const results = await Promise.all(children.map((item) => item.result));
  for (const result of results) assert.equal(result.code, 0, JSON.stringify(result));
  const lines = readFileSync(log, "utf8").trim().split("\n");
  assert.equal(lines.length, 8);
  for (let n = 0; n < lines.length; n += 2) assert.equal(lines[n + 1], lines[n].replace("start", "end"));
});

test("unpublished scope metadata is never mistaken for a stale lease", { timeout: 15000 }, async (t) => {
  const root = await project(t);
  mkdirSync(path.join(root, leaseName));
  const result = await launch(root, command("exit", root, "0")).result;
  assert.equal(result.code, 1);
  assert.match(result.stderr, /metadata|publication/i);
  assert.equal(existsSync(path.join(root, leaseName)), true);
});

test("an interrupted recovery owner fails closed instead of waiting forever", { timeout: 15000 }, async (t) => {
  const { root, record } = await stale(t);
  symlinkSync(JSON.stringify(record.holder), path.join(root, leaseName, "recovering"));
  const result = await launch(root, command("exit", root, "0")).result;
  assert.equal(result.code, 1);
  assert.match(result.stderr, /Interrupted.*recovery/i);
  assert.equal(owner(root).nonce, record.nonce);
});

test("a kernel-visible zombie group cannot be declared absent from procfs state", { timeout: 15000 }, async (t) => {
  const { root, record } = await stale(t);
  const gate = new SharedArrayBuffer(4);
  const worker = new Worker(new URL("./browser-build-fixtures/zombie-owner.mjs", import.meta.url), { workerData: { root, gate } });
  const finished = new Promise((resolve, reject) => { worker.on("exit", resolve); worker.on("error", reject); });
  const pid = await new Promise((resolve, reject) => { worker.once("message", resolve); worker.once("error", reject); });
  t.after(async () => {
    // The worker has not reaped this direct child, so its PID cannot have been
    // recycled. Ensure a fixture failure cannot strand its barrier loop.
    if (processStat(pid)) process.kill(pid, "SIGKILL");
    Atomics.store(new Int32Array(gate), 0, 1);
    Atomics.notify(new Int32Array(gate), 0);
    await finished;
  });
  await until(() => existsSync(path.join(root, "zombie-ready")));
  const identity = processStat(pid);
  writeFileSync(path.join(root, "zombie-exit"), "");
  await until(() => processStat(pid)?.state === "Z");
  assert.equal(process.kill(-pid, 0), true, "actual kernel group probe must still see the zombie");
  record.scope = { leaderPid: pid, group: pid, session: pid, start: identity.start };
  writeFileSync(path.join(root, leaseName, "owner.json"), JSON.stringify(record));
  const result = await launch(root, command("exit", root, "0")).result;
  assert.equal(result.code, 1, JSON.stringify(result));
  assert.match(result.stderr, /unreaped|orphan/);
  assert.equal(processStat(pid)?.state, "Z");
  assert.equal(owner(root).nonce, record.nonce);
});

for (const mode of ["EPERM", "EIO"]) {
  test(`${mode} group proof failures retain the stale lease`, { timeout: 15000 }, async (t) => {
    const { root, record } = await stale(t);
    const preload = new URL("./browser-build-fixtures/scope-probe-error.mjs", import.meta.url).href;
    const result = await launch(root, command("exit", root, "0"), {
      NODE_OPTIONS: `--import=${preload}`,
      LEASE_FIXTURE_SCOPE_PROBE_ROOT: root,
      LEASE_FIXTURE_SCOPE_PROBE_MODE: mode,
    }).result;
    assert.equal(result.code, 1, JSON.stringify(result));
    assert.match(result.stderr, new RegExp(mode));
    assert.equal(owner(root).nonce, record.nonce);
  });
}

test("a normal-release group proof error retains the published lease", { timeout: 15000 }, async (t) => {
  const root = await project(t);
  const preload = new URL("./browser-build-fixtures/scope-probe-error.mjs", import.meta.url).href;
  const result = await launch(root, command("exit", root, "0"), {
    NODE_OPTIONS: `--import=${preload}`,
    LEASE_FIXTURE_SCOPE_PROBE_ROOT: root,
    LEASE_FIXTURE_SCOPE_PROBE_MODE: "EPERM",
  }).result;
  assert.equal(result.code, 1, JSON.stringify(result));
  assert.match(result.stderr, /Cannot prove.*EPERM/);
  assert.equal(owner(root).version, 2);
  assert.equal(existsSync(path.join(root, "negative-probe")), true);
});

for (const group of [0, 1]) {
  test(`invalid process group ${group} never reaches a negative-PID syscall`, { timeout: 15000 }, async (t) => {
    const { root, record } = await stale(t);
    record.scope = { leaderPid: group, group, session: group, start: "0" };
    writeFileSync(path.join(root, leaseName, "owner.json"), JSON.stringify(record));
    const preload = new URL("./browser-build-fixtures/scope-probe-error.mjs", import.meta.url).href;
    const result = await launch(root, command("exit", root, "0"), {
      NODE_OPTIONS: `--import=${preload}`,
      LEASE_FIXTURE_SCOPE_PROBE_ROOT: root,
      LEASE_FIXTURE_SCOPE_PROBE_MODE: "forbid-negative",
    }).result;
    assert.equal(result.code, 1);
    assert.match(result.stderr, /metadata/);
    assert.equal(existsSync(path.join(root, "negative-probe")), false);
    assert.equal(owner(root).nonce, record.nonce);
  });
}

test("a reused scope leader PID fails before a kernel probe", { timeout: 15000 }, async (t) => {
  const { root, record } = await stale(t);
  record.scope = { leaderPid: process.pid, group: process.pid, session: process.pid, start: "0" };
  writeFileSync(path.join(root, leaseName, "owner.json"), JSON.stringify(record));
  const preload = new URL("./browser-build-fixtures/scope-probe-error.mjs", import.meta.url).href;
  const result = await launch(root, command("exit", root, "0"), {
    NODE_OPTIONS: `--import=${preload}`,
    LEASE_FIXTURE_SCOPE_PROBE_ROOT: root,
    LEASE_FIXTURE_SCOPE_PROBE_MODE: "forbid-negative",
  }).result;
  assert.equal(result.code, 1);
  assert.match(result.stderr, /scope PID reuse/);
  assert.equal(existsSync(path.join(root, "negative-probe")), false);
});

test("v1 session-scan metadata is never treated as a v2 group proof", { timeout: 15000 }, async (t) => {
  const { root, record } = await stale(t);
  record.version = 1;
  writeFileSync(path.join(root, leaseName, "owner.json"), JSON.stringify(record));
  const result = await launch(root, command("exit", root, "0")).result;
  assert.equal(result.code, 1);
  assert.match(result.stderr, /proof version/);
  assert.equal(owner(root).nonce, record.nonce);
});

test("unknown host proof refuses recovery", { timeout: 15000 }, async (t) => {
  const { root, record } = await stale(t);
  record.host.boot = "unknown-host-boot";
  writeFileSync(path.join(root, leaseName, "owner.json"), JSON.stringify(record));
  const result = await launch(root, command("exit", root, "0")).result;
  assert.equal(result.code, 1);
  assert.match(result.stderr, /host|namespace|boot/i);
});
