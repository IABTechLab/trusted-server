import fs from "node:fs";
import { syncBuiltinESMExports } from "node:module";
import path from "node:path";
import { setTimeout as delay } from "node:timers/promises";

// Synchronize the old scan after enumeration, or its kernel-probe replacement
// immediately before the actual syscall. No shipping runner test switch exists.
const root = process.env.LEASE_FIXTURE_FORK_RACE_ROOT;
const file = (name) => path.join(root, name);
const originalRead = fs.promises.readdir;
let paused = false;
fs.promises.readdir = async function (directory, ...args) {
  const entries = await originalRead(directory, ...args);
  if (root && directory === "/proc" && process.argv.includes("--lease-holder") && !paused) {
    paused = true;
    fs.writeFileSync(file("proof-ready"), "scan");
    while (!fs.existsSync(file("proof-continue"))) await delay(5);
  }
  return entries;
};
syncBuiltinESMExports();

const originalKill = process.kill;
const outer = process.argv[2] === "run";
let probes = 0;
process.kill = function (pid, signal) {
  if (root && outer && pid < -1 && signal === 0) {
    if (!paused) {
      paused = true;
      fs.writeFileSync(file("proof-ready"), "kernel");
      const sleeper = new Int32Array(new SharedArrayBuffer(4));
      while (!fs.existsSync(file("proof-continue"))) Atomics.wait(sleeper, 0, 0, 5);
    }
    const result = originalKill(pid, signal);
    fs.writeFileSync(file("group-live-probes"), String(++probes));
    return result;
  }
  return originalKill(pid, signal);
};
