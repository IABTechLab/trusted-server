import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { mkdir, open, readFile, readlink, realpath, rename, rm, stat, writeFile, symlink } from "node:fs/promises";
import { constants as osConstants } from "node:os";
import path from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";

// This lease owns one dedicated process group, not every group in a session.
// Commands must not escape with setpgid, setsid, or another PID namespace.
// This is cooperative build coordination, not a cgroup or a sandbox.
export const browserProjectRoot = fileURLToPath(new URL("../crates/trusted-server-js/lib/", import.meta.url));
const tokenName = "TS_BROWSER_BUILD_LEASE";
const leaseName = ".browser-build-lease";
const recordVersion = 2;
// Required stale-absence observation window, not a TTL. Live groups never expire.
const graceMs = 1000;
const pollMs = 50;
const signals = ["SIGINT", "SIGTERM", "SIGHUP"];
const signalCode = (signal) => 128 + osConstants.signals[signal];

async function processInfo(pid) {
  try {
    const stat = await readFile(`/proc/${pid}/stat`, "utf8");
    const fields = stat.slice(stat.lastIndexOf(")") + 2).trim().split(/\s+/);
    return { pid: Number(pid), state: fields[0], parent: Number(fields[1]), group: Number(fields[2]), session: Number(fields[3]), start: fields[19] };
  } catch (error) {
    if (error.code === "ENOENT" || error.code === "ESRCH") return null;
    throw new Error(`Cannot prove process ${pid} identity: ${error.message}`);
  }
}
const alive = (info) => info && info.state !== "Z" && info.state !== "X";

async function hostProof() {
  if (process.platform !== "linux") {
    throw new Error("Browser build lease requires Linux process-group proof; this platform is unsupported");
  }
  const mounts = await readFile("/proc/mounts", "utf8");
  const proc = mounts.split("\n").find((line) => line.split(" ")[1] === "/proc");
  if (!proc || /hidepid=(?!0(?:,|\s))/.test(proc)) {
    throw new Error("Cannot prove browser lease identities: /proc is absent or hides processes");
  }
  return {
    boot: (await readFile("/proc/sys/kernel/random/boot_id", "utf8")).trim(),
    namespace: await readlink("/proc/self/ns/pid"),
  };
}

async function readOwner(lock) {
  try {
    return JSON.parse(await readFile(path.join(lock, "owner.json"), "utf8"));
  } catch (error) {
    if (error.code === "ENOENT") return null;
    throw new Error(`Cannot prove browser lease metadata: ${error.message}`);
  }
}

function validScope(scope) {
  // -1 means every permitted process, not process group 1. Never let it reach
  // process.kill. The detached child in this PID namespace cannot be PID 1.
  return Number.isSafeInteger(scope?.leaderPid) && scope.leaderPid > 1 &&
    scope.group === scope.leaderPid && scope.session === scope.leaderPid &&
    typeof scope.start === "string" && /^\d+$/.test(scope.start);
}

function validateOwner(record, host) {
  if (record?.version !== recordVersion) {
    throw new Error("Unsupported browser lease metadata proof version; expected process-group v2; recovery refused");
  }
  if (!Number.isSafeInteger(record.holder?.pid) || record.holder.pid <= 0 ||
      typeof record.holder.start !== "string" || !/^\d+$/.test(record.holder.start) ||
      !/^[a-f0-9]{64}$/.test(record.nonce) || !validScope(record.scope) ||
      record.holder.pid === record.scope.leaderPid || !Number.isFinite(record.startedAt)) {
    throw new Error("Incomplete browser lease scope metadata; automatic recovery refused");
  }
  if (record.host?.boot !== host.boot || record.host?.namespace !== host.namespace) {
    throw new Error("Browser lease host/boot/PID namespace proof differs; automatic recovery refused");
  }
}

async function holderState(record) {
  const holder = await processInfo(record.holder.pid);
  if (holder && holder.start !== record.holder.start) {
    throw new Error("Browser lease holder PID reuse: process-start identity differs; recovery refused");
  }
  return alive(holder);
}

async function checkScopeIdentity(record) {
  if (!validScope(record.scope)) throw new Error("Invalid dedicated browser process-group identity");
  const leader = await processInfo(record.scope.leaderPid);
  if (leader && (leader.start !== record.scope.start || leader.group !== record.scope.group || leader.session !== record.scope.session)) {
    throw new Error("Browser lease scope PID reuse or changed process identity; recovery refused");
  }
}

async function scopeAbsent(record) {
  await checkScopeIdentity(record);
  try {
    process.kill(-record.scope.group, 0);
    // This includes a group kept alive by zombies. A procfs snapshot must never
    // override the kernel's existence result. Non-reaping hosts can stay blocked.
    return false;
  } catch (error) {
    if (error.code === "ESRCH") return true;
    throw new Error(`Cannot prove browser process-group absence: ${error.message}`);
  }
}

async function isDescendant(holderPid) {
  let current = await processInfo(process.pid);
  const seen = new Set();
  while (current && !seen.has(current.pid)) {
    if (current.pid === holderPid) return true;
    seen.add(current.pid);
    current = await processInfo(current.parent);
  }
  return false;
}

/** Refuse unleased calls and copied tokens outside the live holder's ancestry. */
export async function assertBrowserBuildLease(root = browserProjectRoot) {
  root = await realpath(root);
  const host = await hostProof();
  const record = await readOwner(path.join(root, leaseName));
  if (!record) throw new Error("Browser build lease metadata missing; invoke browser-build-runner.mjs");
  validateOwner(record, host);
  if (!process.env[tokenName] || process.env[tokenName] !== record.nonce) {
    throw new Error("Browser build lease nonce missing or mismatched; nested invocation refused");
  }
  await checkScopeIdentity(record);
  const self = await processInfo(process.pid);
  if (!(await holderState(record)) || self.group !== record.scope.group || self.session !== record.scope.session ||
      !(await isDescendant(record.holder.pid))) {
    throw new Error("Browser build lease token is outside the live holder's scope/ancestry");
  }
  return record;
}

async function retire(lock, record) {
  const retired = `${lock}.retired-${record.nonce}`;
  await rename(lock, retired);
  await rm(retired, { recursive: true });
}

async function recover(lock, record, host, interrupted) {
  if (!(await scopeAbsent(record))) {
    throw new Error("Browser lease holder is dead but its child scope is alive or unreaped; orphan recovery refused");
  }
  let directory;
  try { directory = await open(lock, "r"); } catch (error) {
    if (error.code === "ENOENT") return false;
    throw error;
  }
  // Anchor marker operations to this inode, never a successor at the same path.
  const anchor = `/proc/self/fd/${directory.fd}`;
  let ownsRecovery = false;
  let retired = false;
  try {
    try {
      const self = await processInfo(process.pid);
      await symlink(JSON.stringify({ pid: process.pid, start: self.start }), path.join(anchor, "recovering"));
      ownsRecovery = true;
    } catch (error) {
      if (error.code === "ENOENT") return false;
      if (error.code !== "EEXIST") throw error;
      let reaper;
      try { reaper = JSON.parse(await readlink(path.join(anchor, "recovering"))); } catch (readError) {
        if (readError.code === "ENOENT") return false;
        throw new Error(`Cannot prove recovery owner: ${readError.message}`);
      }
      if (!Number.isSafeInteger(reaper.pid) || reaper.pid <= 0 || !/^\d+$/.test(reaper.start)) {
        throw new Error("Cannot prove recovery owner identity; recovery refused");
      }
      const info = await processInfo(reaper.pid);
      if (!alive(info) || info.start !== reaper.start) {
        throw new Error("Interrupted browser lease recovery; inspect the recorded scope before removing its recovery marker");
      }
      await delay(pollMs);
      return false;
    }
    const current = await readOwner(anchor);
    if (!current || current.nonce !== record.nonce) return false;
    validateOwner(current, host);
    const since = performance.now();
    do {
      if (interrupted()) throw new Error("Browser lease wait interrupted");
      if (await holderState(record) || !(await scopeAbsent(record))) {
        throw new Error("Browser lease holder or child scope is alive during recovery grace; recovery refused");
      }
      await delay(pollMs);
    } while (performance.now() - since < graceMs);
    if (await holderState(record) || !(await scopeAbsent(record))) {
      throw new Error("Browser lease scope exists after recovery grace; recovery refused");
    }
    await retire(lock, record);
    retired = true;
    return true;
  } finally {
    try {
      if (ownsRecovery && !retired) await rm(path.join(anchor, "recovering"), { force: true });
    } finally { await directory.close(); }
  }
}

async function acquire(root, interrupted) {
  const host = await hostProof();
  const self = await processInfo(process.pid);
  const lock = path.join(root, leaseName);
  while (!interrupted()) {
    try {
      await mkdir(lock, { mode: 0o700 });
      // No tools run before a dedicated child reports its identity and the
      // frontend atomically publishes the complete record below.
      return { lock, host, holder: { pid: process.pid, start: self.start }, nonce: randomBytes(32).toString("hex"), startedAt: Date.now() };
    } catch (error) { if (error.code !== "EEXIST") throw error; }
    let directory;
    try { directory = await open(lock, "r"); } catch (error) {
      if (error.code === "ENOENT") continue;
      throw error;
    }
    try {
      const anchor = `/proc/self/fd/${directory.fd}`;
      let record = await readOwner(anchor);
      if (!record) {
        await delay(graceMs);
        record = await readOwner(anchor);
        if (!record) {
          let current;
          try { current = await stat(lock); } catch (error) {
            if (error.code === "ENOENT") continue;
            throw error;
          }
          const original = await directory.stat();
          // A new holder can reuse the path during our publication wait. Its
          // fresh directory is not the old unpublished lease we observed.
          if (current.dev !== original.dev || current.ino !== original.ino) continue;
          throw new Error("Browser lease scope metadata publication incomplete; automatic recovery refused");
        }
      }
      validateOwner(record, host);
      if (await holderState(record)) { await delay(pollMs); continue; }
      await recover(lock, record, host, interrupted);
    } finally { await directory.close(); }
  }
  throw new Error("Browser lease wait interrupted");
}

function commandExit(command, root, env) {
  const child = spawn(command[0], command.slice(1), { cwd: root, env, stdio: "inherit" });
  return new Promise((resolve) => {
    child.on("error", (error) => { console.error(`Browser build command failed: ${error.message}`); resolve(1); });
    child.on("exit", (code, signal) => resolve(code ?? signalCode(signal)));
  });
}

function send(child, message) {
  if (child.connected) child.send(message, () => {});
}

async function hold(command, root) {
  let signal;
  let scope;
  const handlers = new Map(signals.map((name) => [name, () => {
    if (signal) return;
    signal = name;
    // The live child forwards from inside its own group. Once it has exited,
    // never send a signal to a potentially recycled numeric PGID from outside.
    if (scope) send(scope, { type: "signal", signal: name });
  }]));
  for (const [name, handler] of handlers) process.on(name, handler);
  try {
    const acquired = await acquire(root, () => signal);
    scope = spawn(process.execPath, [fileURLToPath(import.meta.url), "--lease-scope"], {
      detached: true, stdio: ["inherit", "inherit", "inherit", "ipc"],
      env: { ...process.env, [tokenName]: "" },
    });
    const result = new Promise((resolve) => {
      scope.on("error", () => resolve(1));
      scope.on("exit", (code, childSignal) => resolve(code ?? signalCode(childSignal)));
    });
    const ready = new Promise((resolve, reject) => {
      scope.once("message", (message) => {
        if (message?.type === "ready") resolve(message.scope);
        else reject(new Error("Invalid browser scope publication handshake"));
      });
      scope.once("error", reject);
      scope.once("exit", () => reject(new Error("Browser scope exited before metadata publication")));
    });
    send(scope, { type: "initialize", root });
    if (signal) send(scope, { type: "signal", signal });
    const identity = await ready;
    const actual = await processInfo(scope.pid);
    if (!validScope(identity) || !actual || identity.leaderPid !== scope.pid ||
        actual.group !== identity.group || actual.session !== identity.session || actual.start !== identity.start) {
      throw new Error("Cannot prove dedicated browser scope before publication");
    }
    const record = { version: recordVersion, holder: acquired.holder, nonce: acquired.nonce, startedAt: acquired.startedAt, host: acquired.host, scope: identity };
    validateOwner(record, acquired.host);
    await writeFile(path.join(acquired.lock, "owner.pending"), JSON.stringify(record), { mode: 0o600 });
    await rename(path.join(acquired.lock, "owner.pending"), path.join(acquired.lock, "owner.json"));
    send(scope, { type: "grant", command, nonce: record.nonce });
    const code = await result;
    while (!(await scopeAbsent(record))) {
      if (signal) {
        throw new Error("Browser lease wait interrupted after scope leader exit; child group still exists, retaining lease");
      }
      await delay(pollMs);
    }
    await retire(acquired.lock, record);
    return signal ? signalCode(signal) : code;
  } catch (error) {
    console.error(`Browser build lease: ${error.message}`);
    // No proof means no release. A pre-publication child may not run any tools.
    if (scope?.connected) scope.disconnect();
    return signal ? signalCode(signal) : 1;
  } finally {
    for (const [name, handler] of handlers) process.off(name, handler);
  }
}

/** Run a browser command in the outer lease or an authenticated nested scope. */
export async function runBrowserBuild(command, { root = browserProjectRoot } = {}) {
  if (!command.length) throw new Error("Expected a browser command after --");
  root = await realpath(root);
  await hostProof();
  const record = await readOwner(path.join(root, leaseName));
  const self = await processInfo(process.pid);
  if (process.env[tokenName] || record?.scope?.session === self.session) {
    await assertBrowserBuildLease(root);
    return commandExit(command, root, process.env);
  }
  return hold(command, root);
}

async function scopeMain() {
  let signal;
  let running = false;
  const forward = (name) => {
    if (signal) return;
    signal = name;
    if (running) process.kill(-process.pid, name);
  };
  for (const name of signals) process.on(name, () => forward(name));
  process.on("message", (message) => {
    if (message?.type === "signal" && signals.includes(message.signal)) forward(message.signal);
  });
  const initial = await new Promise((resolve) => process.once("message", resolve));
  if (initial?.type !== "initialize") throw new Error("Browser scope requires an initialization handshake");
  const self = await processInfo(process.pid);
  const scope = { leaderPid: process.pid, group: self.group, session: self.session, start: self.start };
  if (!validScope(scope)) throw new Error("Browser command scope must lead a dedicated process group/session");
  const grant = new Promise((resolve, reject) => {
    process.on("message", (message) => { if (message?.type === "grant") resolve(message); });
    process.once("disconnect", () => reject(new Error("Browser holder disconnected before granting tools")));
  });
  process.send({ type: "ready", scope });
  const message = await grant;
  process.env[tokenName] = message.nonce;
  await assertBrowserBuildLease(initial.root);
  running = true;
  const code = signal ? signalCode(signal) : await commandExit(message.command, initial.root, process.env);
  running = false;
  if (process.connected) process.disconnect();
  return signal ? signalCode(signal) : code;
}

if (process.argv[1] === fileURLToPath(import.meta.url) && process.argv[2] === "--lease-scope") {
  if (!process.send) throw new Error("Lease scope requires its holder IPC channel");
  try { process.exitCode = await scopeMain(); } catch (error) {
    console.error(`Browser build scope: ${error.message}`);
    if (process.connected) process.disconnect();
    process.exitCode = 1;
  }
}
