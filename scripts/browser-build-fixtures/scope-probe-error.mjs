import fs from "node:fs";
import path from "node:path";

// Fault injection is a test-only preload, never a shipping runner option.
const mode = process.env.LEASE_FIXTURE_SCOPE_PROBE_MODE;
const root = process.env.LEASE_FIXTURE_SCOPE_PROBE_ROOT;
const kill = process.kill;
process.kill = function (pid, signal) {
  if (mode && pid < 0) {
    fs.writeFileSync(path.join(root, "negative-probe"), String(pid));
    if (mode === "forbid-negative") throw new Error("Invalid metadata reached negative PID syscall");
    if (signal === 0) throw Object.assign(new Error(`injected ${mode}`), { code: mode });
  }
  return kill(pid, signal);
};
