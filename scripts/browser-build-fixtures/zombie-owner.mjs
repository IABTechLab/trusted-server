import { spawn } from "node:child_process";
import { parentPort, workerData } from "node:worker_threads";
import { fileURLToPath } from "node:url";

// Keep this worker's event loop from reaping its direct child until the test
// has observed the real kernel-visible zombie group from another thread.
const child = spawn(process.execPath, [fileURLToPath(new URL("./process.mjs", import.meta.url)), "zombie", workerData.root], {
  detached: true,
  stdio: "ignore",
});
child.on("error", (error) => { throw error; });
child.on("spawn", () => {
  parentPort.postMessage(child.pid);
  Atomics.wait(new Int32Array(workerData.gate), 0, 0);
});
child.on("exit", () => parentPort.close());
